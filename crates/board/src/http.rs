//! HTTP surface: the JSON snapshot, the SSE stream, and static UI serving.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse,
    },
    routing::get,
    Json, Router,
};
use chrono::Utc;
use collector::{Event, Session, SessionStore};
use futures::StreamExt;
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Shared state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<SessionStore>>,
    pub tx: broadcast::Sender<Event>,
    pub web_dist: PathBuf,
}

/// The plain-text message shown when the UI has not been built yet.
const WEB_DIST_MISSING: &str =
    "web/dist not found — run: cd web && npm install && npm run build";

/// Build the full application router.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/events", get(events))
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

/// `GET /api/sessions` — full snapshot; the client upserts by `id`.
async fn sessions(State(state): State<AppState>) -> Json<SessionsResponse> {
    let sessions = {
        let store = state.store.lock().unwrap();
        store.snapshot(Utc::now())
    };
    Json(SessionsResponse { sessions })
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
