//! The Relay: fan-in from Collectors, fan-out to boards.
//!
//! It keeps only in-memory live state (ADR 0004): a merged map of every connected
//! Collector's current Sessions, keyed by session id. A new board subscriber is
//! served that map as a snapshot, then a live stream — the same "snapshot then
//! stream" shape the board offers locally. The Relay is strictly one-way (ADR
//! 0002): it transports session state, never commands.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        sse::{KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Router,
};
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, warn};

use crate::wire::{authorized, speaks_v2, to_sse, NdjsonDecoder, WireEvent, WireSession};

/// One entry in the merged map: the wire session plus the id of the Collector
/// connection that last wrote it. The owner guards the disconnect race — a dropped
/// connection only reaps sessions it still owns, so a Collector that reconnects on a
/// fresh connection before the old one's cleanup runs is not clobbered. The Relay
/// stores the wire type verbatim and never interprets Attention (ADR 0001/0004/0010).
struct Owned {
    conn: u64,
    session: WireSession,
}

/// Shared Relay state: the token gate, the merged live map, and the board fan-out.
#[derive(Clone)]
pub struct RelayState {
    token: Arc<str>,
    sessions: Arc<Mutex<HashMap<String, Owned>>>,
    conns: Arc<AtomicU64>,
    tx: broadcast::Sender<WireEvent>,
}

impl RelayState {
    pub fn new(token: impl Into<String>) -> Self {
        let (tx, _) = broadcast::channel(1024);
        RelayState {
            token: Arc::from(token.into()),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            conns: Arc::new(AtomicU64::new(0)),
            tx,
        }
    }

    /// A unique id for a new Collector connection.
    fn next_conn(&self) -> u64 {
        self.conns.fetch_add(1, Ordering::Relaxed)
    }

    /// Record a wire session as owned by `conn` (last writer wins).
    fn upsert(&self, conn: u64, session: WireSession) {
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Owned { conn, session });
    }

    /// Drop `id` iff `conn` still owns it. Returns whether it was actually removed,
    /// so the caller only fans a `Removed` out when it reflects real state.
    fn remove_if_owner(&self, conn: u64, id: &str) -> bool {
        let mut map = self.sessions.lock().unwrap();
        match map.get(id) {
            Some(o) if o.conn == conn => {
                map.remove(id);
                true
            }
            _ => false,
        }
    }

    /// The current merged state as `Upsert` events — a subscriber's snapshot.
    fn snapshot(&self) -> Vec<WireEvent> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|o| WireEvent::Upsert(o.session.clone()))
            .collect()
    }
}

/// The Relay HTTP router: `POST /collect` (push), `GET /subscribe` (fan-out), and a
/// `GET /health` liveness probe that needs no token.
pub fn router(state: RelayState) -> Router {
    Router::new()
        .route("/collect", post(collect))
        .route("/subscribe", get(subscribe))
        .route("/health", get(|| async { "ok" }))
        .with_state(state)
}

/// `POST /collect` — a Collector's long-lived push. The body is an NDJSON stream of
/// `Event`s. Each is merged into the live map and fanned out to every board. When
/// the connection ends (the Collector went offline or the network dropped), every
/// session this connection still owns is removed so boards clear its stale cards
/// (User Story 7); on reconnect the Collector re-pushes its current state.
async fn collect(
    State(state): State<RelayState>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return StatusCode::UNAUTHORIZED;
    }

    let conn = state.next_conn();
    // Negotiate the Attention protocol: a Collector that does not advertise the
    // capability is legacy, and its Attention degrades downstream (ADR 0010). The
    // Relay itself never interprets Attention — it forwards the wire verbatim.
    let legacy = !speaks_v2(&headers);
    info!(conn, legacy, "collector connected");
    let mut stream = body.into_data_stream();
    let mut decoder = NdjsonDecoder::default();
    // The session ids this connection has upserted and not yet removed — the set to
    // reap when it disconnects.
    let mut owned: HashSet<String> = HashSet::new();

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                warn!(conn, error = %e, "collector stream error");
                break;
            }
        };
        for event in decoder.push(&bytes) {
            match event {
                WireEvent::Upsert(session) => {
                    owned.insert(session.id.clone());
                    state.upsert(conn, session.clone());
                    let _ = state.tx.send(WireEvent::Upsert(session));
                }
                WireEvent::Removed { id } => {
                    owned.remove(&id);
                    if state.remove_if_owner(conn, &id) {
                        let _ = state.tx.send(WireEvent::Removed { id });
                    }
                }
            }
        }
    }

    for id in owned {
        if state.remove_if_owner(conn, &id) {
            let _ = state.tx.send(WireEvent::Removed { id });
        }
    }
    StatusCode::OK
}

/// `GET /subscribe` — a board's fan-out stream (SSE). The subscriber receives the
/// current merged snapshot, then live `Event`s. Subscribing to the broadcast before
/// snapshotting closes the gap where an update could slip between the two; a
/// resulting duplicate is harmless because upserts are idempotent.
async fn subscribe(State(state): State<RelayState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // A board that does not advertise the capability is a legacy subscriber; it
    // simply ignores the additive `attention` field. Negotiation is recorded so the
    // contract is explicit (ADR 0010), while the Relay stays an uninterpreting
    // transport — it forwards the wire session verbatim.
    info!(v2 = speaks_v2(&headers), "board subscribed");
    let rx = state.tx.subscribe();
    let snapshot = state.snapshot();
    let snapshot = futures::stream::iter(
        snapshot
            .into_iter()
            .map(|e| Ok::<_, Infallible>(to_sse(&e))),
    );
    let live = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(event) => Some(Ok::<_, Infallible>(to_sse(&event))),
            // Lagged: drop it. The board re-syncs on reconnect (snapshot-on-connect),
            // and every event is a full snapshot, so a gap self-heals.
            Err(_) => None,
        }
    });

    Sse::new(snapshot.chain(live))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

/// Bind `addr` and serve the Relay until the process is killed.
pub async fn run(addr: SocketAddr, token: String) -> std::io::Result<()> {
    let app = router(RelayState::new(token));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Relay listening on http://{addr}");
    axum::serve(listener, app).await
}
