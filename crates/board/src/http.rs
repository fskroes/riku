//! HTTP surface: the JSON snapshot, the SSE stream, and static UI serving.

use std::collections::HashSet;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use collector::{DeepLink, Event, Session, SessionStore, Status, Tool, WorkItem, WorkSourceKind};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::open::{is_safe_session_id, Launcher};
use crate::runtime::{RelayStatus, RemoteSessions};

/// Shared state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<SessionStore>>,
    pub tx: broadcast::Sender<Event>,
    pub web_dist: PathBuf,
    /// Live git `+/-` per repo, filled onto session cards at the output boundary.
    pub diff_cache: Arc<collector::DiffCache>,
    /// How the board opens a local session (a terminal launch); injectable so
    /// tests can record the deep link instead of spawning Terminal.
    pub launcher: Arc<dyn Launcher>,
    /// This machine's name, stamped onto every local session and Work Link (C7) so
    /// each card shows which machine it is on.
    pub machine: Arc<str>,
    /// Sessions relayed from other machines (C7), merged with local sessions at the
    /// snapshot boundary. Empty on a local-only board.
    pub remote: RemoteSessions,
    /// The board's Relay-subscription state, for the topbar pill.
    pub relay_status: Arc<RelayStatus>,
}

/// The plain-text message shown when the UI has not been built yet.
const WEB_DIST_MISSING: &str =
    "web/dist not found — run: cd web && npm install && npm run build";

/// Build the full application router.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/:id/open", post(open_session))
        .route("/api/events", get(events))
        .route("/api/work", get(work))
        .route("/api/relay", get(relay_status))
        .with_state(state.clone());

    let dist = state.web_dist;
    if dist.is_dir() && dist.join("index.html").is_file() {
        // Serve built assets; fall back to index.html for the SPA.
        let index = tower_http::services::ServeFile::new(dist.join("index.html"));
        let serve = tower_http::services::ServeDir::new(&dist).not_found_service(index);
        api.fallback_service(serve)
    } else {
        api.fallback(missing_dist)
    }
}

#[derive(Serialize)]
struct SessionsResponse {
    sessions: Vec<Session>,
}

/// `GET /api/sessions` — full snapshot; the client upserts by `id`. Local cards are
/// enriched with their live git `+/-` off the async workers (git can block) and
/// stamped with this machine's name. Remote cards (C7) are merged in as-is — the
/// Collector that relayed them already enriched and stamped them on their own
/// machine — so a late-connecting browser sees the full multi-machine picture from
/// its first fetch, not only once each remote session next updates.
async fn sessions(State(state): State<AppState>) -> Json<SessionsResponse> {
    let mut sessions = {
        let store = state.store.lock().unwrap();
        store.snapshot(Utc::now())
    };
    let cache = state.diff_cache.clone();
    let machine = state.machine.clone();
    let mut sessions = tokio::task::spawn_blocking(move || {
        for s in &mut sessions {
            cache.enrich(s);
            crate::runtime::stamp_local(s, &machine);
        }
        sessions
    })
    .await
    .expect("diff enrichment does not panic");

    // Merge in remote sessions, keeping a local session when an id somehow appears
    // in both (this board also collects the same machine): local carries live git.
    let local_ids: HashSet<String> = sessions.iter().map(|s| s.id.clone()).collect();
    for s in state.remote.lock().unwrap().values() {
        if !local_ids.contains(&s.id) {
            sessions.push(s.clone());
        }
    }
    Json(SessionsResponse { sessions })
}

/// The board's Relay-subscription status, for the topbar pill (C7). `configured` is
/// whether a Relay was set up at all (else zero-setup solo mode); `connected` is
/// whether the subscription is currently live (else reconnecting).
#[derive(Serialize)]
struct RelayStatusResponse {
    configured: bool,
    connected: bool,
}

/// `GET /api/relay` — the current Relay subscription status.
async fn relay_status(State(state): State<AppState>) -> Json<RelayStatusResponse> {
    Json(RelayStatusResponse {
        configured: state.relay_status.configured,
        connected: state.relay_status.connected.load(Ordering::Relaxed),
    })
}

/// `POST /api/sessions/:id/open` — deep-link into the local session (ADR 0002).
///
/// The board is local, so this genuinely opens a terminal on the human's machine,
/// resuming the exact session. The only client input is `id`; the tool, working
/// directory, and transcript all come from the store, so the caller cannot point
/// the launch at an arbitrary command or directory. `404` if no live session has
/// that id; `422` if it has no known `cwd` to resume into; `502` if the launch
/// itself fails (the launcher's reason is passed through for the UI to show).
async fn open_session(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    if !is_safe_session_id(&id) {
        return error(StatusCode::BAD_REQUEST, "not a valid session id");
    }

    let resolved = {
        let store = state.store.lock().unwrap();
        store.find_by_id(&id, Utc::now())
    };
    let Some((transcript, session)) = resolved else {
        return error(StatusCode::NOT_FOUND, "no live session with that id");
    };

    let Some(link) = DeepLink::resume(session.tool, &session.id, session.cwd.as_deref(), &transcript)
    else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "this session has no known working directory to open",
        );
    };

    // The launch shells out to `osascript`; keep it off the async runtime.
    let launcher = state.launcher.clone();
    let dir = link.dir.to_string_lossy().to_string();
    let result = tokio::task::spawn_blocking(move || launcher.open(&link))
        .await
        .expect("launcher does not panic");

    match result {
        Ok(()) => (StatusCode::OK, Json(json!({ "opened": true, "dir": dir }))).into_response(),
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

/// A `{ "error": <message> }` body at `status`, for the UI to surface.
fn error(status: StatusCode, message: &str) -> axum::response::Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[derive(Deserialize)]
struct WorkQuery {
    /// The project directory (a session's `cwd`). Scoped to known sessions so the
    /// endpoint cannot be pointed at an arbitrary path.
    cwd: String,
}

/// The Work Items for one project, plus the source they came from. Each item may
/// carry the Agent Session working it (the Work Link, inferred from the branch).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkResponse {
    project: String,
    source: WorkSourceKind,
    items: Vec<WorkItemOut>,
}

/// A Work Item enriched with its Work Link — the [`LinkedSession`] whose branch
/// this item's id was inferred from, if any.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemOut {
    #[serde(flatten)]
    item: WorkItem,
    session: Option<LinkedSession>,
}

/// The compact session reference shown as an inset chip on an In-progress item.
/// Carries `id` so the chip can cross-link to the same session's card on the Board.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkedSession {
    id: String,
    project: String,
    tool: Tool,
    model: Option<String>,
    branch: Option<String>,
    status: Status,
    /// The machine the linked session is on (C7). A Work Link is always to a local
    /// session, so this is the board's own host; carried so the chip in the Work
    /// Items view labels it consistently with the same session's card on the Board.
    machine: Option<String>,
}

/// `GET /api/work?cwd=<dir>` — the Work Items for the project rooted at `cwd`.
///
/// `cwd` must match a known session's directory (404 otherwise), which both
/// disambiguates same-named projects and keeps the file/`gh` read scoped to
/// directories the board already watches. The `WORK.md`/`gh` read runs on a
/// blocking thread; the session store lock is released before it starts.
async fn work(State(state): State<AppState>, Query(q): Query<WorkQuery>) -> impl IntoResponse {
    // Snapshot the sessions in this project, releasing the lock before the
    // (potentially slow) source read. Only same-cwd sessions are Work-Link
    // candidates: a branch belongs to one repo.
    let candidates: Vec<Session> = {
        let store = state.store.lock().unwrap();
        store
            .snapshot(Utc::now())
            .into_iter()
            .filter(|s| s.cwd.as_deref() == Some(q.cwd.as_str()))
            .collect()
    };
    let Some(project) = candidates.first().map(|s| s.project.clone()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let dir = PathBuf::from(&q.cwd);
    let map = tokio::task::spawn_blocking(move || collector::read_work_map(&dir))
        .await
        .expect("read_work_map does not panic");

    let items = map
        .items
        .into_iter()
        .map(|item| {
            let session = link_session(&item, &candidates, &state.machine);
            WorkItemOut { item, session }
        })
        .collect();

    Json(WorkResponse {
        project,
        source: map.source,
        items,
    })
    .into_response()
}

/// The Work Link for `item`: the most-recently-active candidate session whose
/// branch the item's id can be inferred from. `None` if nothing links.
fn link_session(item: &WorkItem, candidates: &[Session], machine: &str) -> Option<LinkedSession> {
    candidates
        .iter()
        .filter(|s| s.branch.as_deref().is_some_and(|b| branch_links(b, &item.id)))
        .max_by_key(|s| s.last_event_at)
        .map(|s| LinkedSession {
            id: s.id.clone(),
            project: s.project.clone(),
            tool: s.tool,
            model: s.model.clone(),
            branch: s.branch.clone(),
            status: s.status,
            // Snapshot candidates are unstamped; a Work Link is local, so use this
            // machine's name (falling back to the session's own tag if present).
            machine: s.machine.clone().or_else(|| Some(machine.to_string())),
        })
}

/// Whether a git branch carries a Work Item's id — the branch-name half of Work
/// Link inference. The id core (`w-14`, or `42` from `#42`) must appear as a token
/// bounded by non-alphanumerics, so `w-14` matches `feature/w-14-x` but not
/// `w-141`, and `#42` matches `fix/42-x` but not inside `142`/`420`. A `W-nn` id
/// also matches its dashless form (`w14`) for branches that drop the dash.
fn branch_links(branch: &str, id: &str) -> bool {
    let branch = branch.to_ascii_lowercase();
    let core = id.trim_start_matches('#').to_ascii_lowercase();
    if core.is_empty() {
        return false;
    }
    contains_token(&branch, &core)
        || (core.contains('-') && contains_token(&branch, &core.replace('-', "")))
}

/// Whether `needle` occurs in `hay` bounded by non-alphanumeric characters (or
/// string ends) on both sides — a whole-token match, not a loose substring.
fn contains_token(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// `GET /api/events` — SSE. `session` events carry a full Session; `removed`
/// events carry `{ "id": ... }`. A `: ping` comment every 15s keeps the
/// connection warm and lets the client detect a dead stream.
async fn events(State(state): State<AppState>) -> impl IntoResponse {
    let rx = state.tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(event) => Some(Ok::<_, Infallible>(to_sse(event))),
            // Lagged: drop it. The client re-syncs via /api/sessions on reconnect,
            // and every event is a full snapshot, so a gap is self-healing.
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

fn to_sse(event: Event) -> SseEvent {
    match event {
        Event::Upsert(session) => SseEvent::default()
            .event("session")
            .json_data(session)
            .expect("Session serializes"),
        Event::Removed { id } => SseEvent::default()
            .event("removed")
            .json_data(json!({ "id": id }))
            .expect("removal payload serializes"),
    }
}

async fn missing_dist() -> impl IntoResponse {
    (StatusCode::SERVICE_UNAVAILABLE, WEB_DIST_MISSING)
}

#[cfg(test)]
mod tests {
    use super::branch_links;

    #[test]
    fn work_map_id_links_by_dash_or_compact_form() {
        assert!(branch_links("feature/W-12-download-flow", "W-12"));
        assert!(branch_links("fix/w12", "W-12"));
        assert!(branch_links("W-12", "W-12"));
        assert!(!branch_links("feature/W-121", "W-12")); // compact wins, but dashless 121 ≠ w-12
        assert!(!branch_links("main", "W-12"));
    }

    #[test]
    fn github_number_links_only_when_digit_bounded() {
        assert!(branch_links("fix/42-thing", "#42"));
        assert!(branch_links("issue-42", "#42"));
        assert!(!branch_links("fix/142-thing", "#42")); // inside a larger number
        assert!(!branch_links("fix/420", "#42"));
        assert!(!branch_links("main", "#42"));
    }
}
