//! HTTP surface: the JSON snapshot, the SSE stream, and static UI serving.

use std::collections::HashSet;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode, Uri},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures::StreamExt;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sessions::{
    DeepLink, Event, Handoff, Session, Status, Tool, WorkItem, WorkSourceKind, WorkStatus,
};
use tokio_stream::wrappers::BroadcastStream;

use crate::open::{is_safe_session_id, Launcher};
use crate::runtime::{BoardEvents, RelayStatus, RemoteSessions};
use session_engine::Engine;

/// Shared state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    /// The shared local-session pipeline. Its interface hides discovery,
    /// filesystem watching, status refresh, git enrichment, stamping, and the
    /// broadcast wiring from this HTTP adapter.
    pub engine: Arc<Engine>,
    /// Board-facing event stream: local Engine events plus Relay events.
    pub events: BoardEvents,
    /// A contributor-provided UI directory. When absent, the compiled-in UI is
    /// served, so an installed binary never depends on its current directory.
    pub web_dist: Option<PathBuf>,
    /// How the board opens a local session (a terminal launch); injectable so
    /// tests can record the deep link instead of spawning Terminal.
    pub launcher: Arc<dyn Launcher>,
    /// Sessions relayed from other machines (C7), merged with local sessions at the
    /// snapshot boundary. Empty on a local-only board.
    pub remote: RemoteSessions,
    /// The board's Relay-subscription state, for the topbar pill.
    pub relay_status: Arc<RelayStatus>,
    /// Whether the Project Journal is switched on (`journal.enabled`, ADR 0013).
    /// Off by default and resolved by `riku` from the user's config, because the
    /// board crate has no config of its own to read. While off, no journal file
    /// is opened at all.
    pub journal_enabled: bool,
    /// Where the journal files live. When absent, the directory Riku owns —
    /// injectable so a fixture journal can be served from a temporary directory
    /// without the process-wide `$XDG_DATA_HOME` an in-process test cannot
    /// isolate.
    pub journal_dir: Option<PathBuf>,
}

/// The plain-text message shown when the UI has not been built yet.
const WEB_DIST_MISSING: &str = "web/dist not found — run: cd web && npm install && npm run build";

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct WebAssets;

/// Build the full application router.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/:id/open", post(open_session))
        .route("/api/events", get(events))
        .route("/api/work", get(work))
        .route("/api/recap", get(recap))
        .route("/api/recap/note", post(note))
        .route("/api/relay", get(relay_status))
        .with_state(state.clone());

    match state.web_dist {
        Some(dist) if dist.is_dir() && dist.join("index.html").is_file() => {
            // The explicit development override retains Vite's hot-reload loop.
            let index = tower_http::services::ServeFile::new(dist.join("index.html"));
            let serve = tower_http::services::ServeDir::new(&dist).not_found_service(index);
            api.fallback_service(serve)
        }
        Some(_) => api.fallback(missing_dist),
        None => api.fallback(embedded_ui),
    }
}

/// Serve a compile-time UI asset, with `index.html` as the SPA fallback. Only the
/// explicitly requested `--web-dist` path can produce the old 503 developer hint.
async fn embedded_ui(uri: Uri) -> axum::response::Response {
    let requested = uri.path().trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let (asset, content_path) = WebAssets::get(requested)
        .map(|asset| (asset, requested))
        .or_else(|| WebAssets::get("index.html").map(|asset| (asset, "index.html")))
        .expect("build guard ensures the embedded index.html exists");
    let mime = mime_guess::from_path(content_path).first_or_octet_stream();
    (
        [(header::CONTENT_TYPE, mime.as_ref())],
        axum::body::Body::from(asset.data.into_owned()),
    )
        .into_response()
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
    let engine = state.engine.clone();
    let mut sessions = tokio::task::spawn_blocking(move || engine.snapshot())
        .await
        .expect("session snapshot does not panic");

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

    let resolved = state.engine.find_by_id(&id);
    let Some((transcript, session)) = resolved else {
        return error(StatusCode::NOT_FOUND, "no live session with that id");
    };

    let Some(link) = DeepLink::resume(
        session.tool,
        &session.id,
        session.cwd.as_deref(),
        &transcript,
    ) else {
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
///
/// The flattened `status` is the one the board shows, which a live Work Link can
/// raise to Doing (see [`status_with_work_link`]); `sourceStatus` is what the
/// `WORK.md` marker or GitHub label actually said. Both travel so the card can
/// disclose the difference — the plan's own word is never quietly overwritten.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemOut {
    #[serde(flatten)]
    item: WorkItem,
    source_status: WorkStatus,
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
    let candidates = state.engine.sessions_in(&q.cwd);
    let Some(project) = candidates.first().map(|s| s.project.clone()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let dir = PathBuf::from(&q.cwd);
    let map = tokio::task::spawn_blocking(move || sessions::read_work_map(&dir))
        .await
        .expect("read_work_map does not panic");

    let items = map
        .items
        .into_iter()
        .map(|mut item| {
            let session = link_session(&item, &candidates);
            let source_status = item.status;
            item.status = status_with_work_link(source_status, session.as_ref().map(|s| s.status));
            WorkItemOut {
                item,
                source_status,
                session,
            }
        })
        .collect();

    Json(WorkResponse {
        project,
        source: map.source,
        items,
    })
    .into_response()
}

/// The status the board shows for a Work Item, given the status of its Work Link
/// (`None` when nothing links).
///
/// A source has no way to say "an agent is on this right now" — Doing exists only
/// as a hand-written `WORK.md` marker or a GitHub label, so an item being worked
/// sat in To do until somebody remembered to mark it (#66). A **live** Work Link
/// answers that from evidence instead.
///
/// Live means Active or Attention: an agent waiting on the user is still carrying
/// the item, and it is the common mid-task state, so excluding it would drop the
/// card back to To do every time the agent asked a question. Finished is not live —
/// a Work Link is inferred from the branch alone and outlives the session's own
/// activity, so "has a Work Link" can never mean "is being worked". Done is left
/// alone: the source asserted completion, and work continuing on the branch
/// afterwards must not un-complete the item.
fn status_with_work_link(source: WorkStatus, link: Option<Status>) -> WorkStatus {
    let live = matches!(link, Some(Status::Active) | Some(Status::Attention));
    if live && source == WorkStatus::Todo {
        WorkStatus::Doing
    } else {
        source
    }
}

/// The Work Link for `item`: the most-recently-active candidate session whose
/// branch the item's id can be inferred from. `None` if nothing links.
fn link_session(item: &WorkItem, candidates: &[Session]) -> Option<LinkedSession> {
    candidates
        .iter()
        .filter(|s| {
            s.branch
                .as_deref()
                .is_some_and(|b| branch_links(b, &item.id))
        })
        .max_by_key(|s| s.last_event_at)
        .map(|s| LinkedSession {
            id: s.id.clone(),
            project: s.project.clone(),
            tool: s.tool,
            model: s.model.clone(),
            branch: s.branch.clone(),
            status: s.status,
            // Candidates come machine-stamped from the Engine (every read is), so the
            // Work Link's machine is just the session's own tag.
            machine: s.machine.clone(),
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

/// `GET /api/recap` — the journal-derived recap: one card per project the board
/// knows a session for, ordered needs-you → needs-review → on-track (ADR 0013).
///
/// Everything the payload says in prose was written by somebody else — the agent
/// at its stop hook, or the user answering it — so it rides out as inert data and
/// is never interpreted here. The resume command is the exception that proves it:
/// it is not carried by the record at all but assembled from the session the
/// store resolved, and shown for a human to copy rather than run (ADR 0002).
///
/// Reading a journal is filesystem work and `snapshot` can invoke git, so the
/// whole assembly runs on a blocking worker.
async fn recap(State(state): State<AppState>) -> Json<crate::recap::Recap> {
    let engine = state.engine.clone();
    let enabled = state.journal_enabled;
    let dir = state.journal_dir.clone();
    let recap = tokio::task::spawn_blocking(move || {
        let sessions = engine.snapshot();
        crate::recap::recap(
            &sessions,
            enabled,
            |project| match &dir {
                Some(dir) => sessions::read_journal_in(dir, project),
                None => sessions::read_journal(project),
            },
            || match &dir {
                Some(dir) => sessions::list_journals_in(dir),
                None => sessions::list_journals(),
            },
            |id| engine.find_by_id(id),
            chrono::Utc::now(),
        )
    })
    .await
    .expect("recap assembly does not panic");
    Json(recap)
}

/// The user's answer to a card, as the correction box sends it.
#[derive(Deserialize)]
struct NoteRequest {
    /// The project directory the card is keyed on — the card's own `cwd`, never
    /// a path the caller thought up, because the endpoint only writes for a
    /// project the board is showing.
    cwd: String,
    /// The user's words. They become the entry's next step, which is the field
    /// latest-wins resolution reads.
    text: String,
    /// Where the user is leaving the card. Optional, falling back to the one
    /// [`Handoff::NOTE_DEFAULT`] `riku journal note` falls back to; a user who
    /// says "that's fine, carry on" says so by naming a calmer one.
    handoff: Option<Handoff>,
}

/// `POST /api/recap/note` — append the user's answer to a card's journal (ADR
/// 0013).
///
/// The one write on the board's surface, and it is the user's own voice: Riku is
/// acting as the user's pen on an explicit user action, never narrating state of
/// its own, so its read-only posture survives. It goes through the same append
/// path as `riku journal note` — one line, appended, file created `0600`,
/// rotated at the cap — because a correction from the card and a correction from
/// the terminal are the same correction.
///
/// Three refusals, in the order they matter. `409` while `journal.enabled` is
/// false: off is off in both directions, and accepting a note would create the
/// very file nobody opted into. `404` for a directory no session on the board is
/// in, which scopes the write to cards the user can actually see, the way
/// `GET /api/work` scopes its read. `400` for a note with nothing in it, the
/// same refusal the CLI gives.
async fn note(
    State(state): State<AppState>,
    Json(request): Json<NoteRequest>,
) -> impl IntoResponse {
    if !state.journal_enabled {
        return error(
            StatusCode::CONFLICT,
            "the journal is off; run 'riku config set journal.enabled true' to turn it on",
        );
    }
    if state.engine.sessions_in(&request.cwd).is_empty() {
        return error(
            StatusCode::NOT_FOUND,
            "no project on the board has that working directory",
        );
    }
    // Trimmed, because the box submits whatever was typed into it and a note of
    // blank space would win latest-wins while saying nothing.
    let text = request.text.trim().to_string();
    if text.is_empty() {
        return error(StatusCode::BAD_REQUEST, "a journal note needs text to say");
    }

    // The slug the hook writes under and Riku reads back, derived from the
    // card's directory — the record is never filed under anything the request
    // named directly.
    let project = sessions::project_slug(std::path::Path::new(&request.cwd));
    let handoff = request.handoff.unwrap_or(Handoff::NOTE_DEFAULT);
    let dir = state.journal_dir.clone();
    let now = chrono::Utc::now();
    let noted = tokio::task::spawn_blocking(move || match &dir {
        Some(dir) => sessions::append_note_in(dir, &project, &text, handoff, now),
        None => sessions::append_note(&project, &text, handoff),
    })
    .await
    .expect("the journal append does not panic");

    match noted {
        // The thread the note answered rides back with the acknowledgement: the
        // append picks it by implication — whoever spoke last — and an endpoint
        // that reports which thread it wrote to is one a caller can check,
        // rather than one that has to be trusted. The card does not need it (it
        // is the thread), which is why it is reported and not required.
        Ok(noted) => (
            StatusCode::OK,
            Json(json!({ "noted": true, "session": noted.session })),
        )
            .into_response(),
        Err(message) => error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

/// `GET /api/events` — SSE. `session` events carry a full Session; `removed`
/// events carry `{ "id": ... }`. A `: ping` comment every 15s keeps the
/// connection warm and lets the client detect a dead stream.
async fn events(State(state): State<AppState>) -> impl IntoResponse {
    let rx = state.events.subscribe();
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
    use super::{branch_links, status_with_work_link};
    use sessions::{Status, WorkStatus};

    #[test]
    fn a_live_work_link_carries_an_unmarked_item_into_doing() {
        // The #66 fix: nobody wrote the marker, but an agent is on it.
        assert_eq!(
            status_with_work_link(WorkStatus::Todo, Some(Status::Active)),
            WorkStatus::Doing
        );
        // An agent waiting on the user is still working the item — and this is the
        // common mid-task state, so excluding it would flap the card back to To do
        // every time the agent asks a question.
        assert_eq!(
            status_with_work_link(WorkStatus::Todo, Some(Status::Attention)),
            WorkStatus::Doing
        );
    }

    #[test]
    fn a_session_that_has_gone_quiet_does_not_claim_the_item() {
        // A Work Link survives the session going Finished (it is inferred from the
        // branch alone), so "has a Work Link" cannot mean "is being worked".
        assert_eq!(
            status_with_work_link(WorkStatus::Todo, Some(Status::Finished)),
            WorkStatus::Todo
        );
        assert_eq!(
            status_with_work_link(WorkStatus::Todo, None),
            WorkStatus::Todo
        );
    }

    #[test]
    fn a_done_item_stays_done_however_live_its_branch() {
        // Done is an explicit assertion of completion by the source. Work continuing
        // on the branch afterwards (review fixes) must not un-complete the item.
        assert_eq!(
            status_with_work_link(WorkStatus::Done, Some(Status::Active)),
            WorkStatus::Done
        );
        assert_eq!(
            status_with_work_link(WorkStatus::Done, Some(Status::Attention)),
            WorkStatus::Done
        );
    }

    #[test]
    fn the_source_saying_doing_needs_no_live_session_to_stay_doing() {
        // The reverse mismatch, which the card already narrates as "In progress ·
        // no live session": the source's word stands on its own.
        assert_eq!(
            status_with_work_link(WorkStatus::Doing, None),
            WorkStatus::Doing
        );
        assert_eq!(
            status_with_work_link(WorkStatus::Doing, Some(Status::Finished)),
            WorkStatus::Doing
        );
    }

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
