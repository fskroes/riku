//! Board-specific adapter around the shared local-session Engine.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use session_engine::local_hostname;
use session_engine::Engine;
use sessions::{Event, Session};
use tokio::sync::broadcast;

use crate::http::AppState;
use crate::open::TerminalLauncher;

/// Where to subscribe for remote sessions. Absent means local-only, zero-setup
/// solo mode.
pub struct RelayConfig {
    pub url: String,
    pub token: String,
}

/// The board's live Relay-subscription state, surfaced to the UI for the topbar
/// pill. `configured` is fixed; `connected` flips as the subscription reconnects.
pub struct RelayStatus {
    pub configured: bool,
    pub connected: AtomicBool,
}

impl RelayStatus {
    fn new(configured: bool) -> Self {
        Self {
            configured,
            connected: AtomicBool::new(false),
        }
    }
}

/// Remote sessions relayed from other machines, held apart from the local Engine
/// and merged only at the Board's HTTP output boundary.
pub type RemoteSessions = Arc<Mutex<HashMap<String, Session>>>;

/// Board-facing event bus. It carries local Engine events plus Relay events, while
/// the Engine itself remains scoped to local-session production.
pub type BoardEvents = broadcast::Sender<Event>;

/// A running board. The shared Engine is retained through `state`, keeping its
/// watcher and refresh loop alive for as long as the server is alive.
pub struct Started {
    pub state: AppState,
}

/// Start the local Engine, then add only Board concerns: HTTP dependencies and an
/// optional Relay subscription. The Engine owns discovery, watch, refresh, diff
/// enrichment, machine stamping, and the local event stream.
///
/// `journal_enabled` is the `journal.enabled` toggle (ADR 0013), passed in rather
/// than read: the board crate has no config of its own, and `riku` is what knows
/// the user's. The journal *directory* is not a parameter — it has a real default
/// and is overridden on [`Started::state`] the way the launcher is.
pub fn init(
    claude_root: PathBuf,
    codex_root: Option<PathBuf>,
    web_dist: Option<PathBuf>,
    relay: Option<RelayConfig>,
    journal_enabled: bool,
) -> Started {
    // Machine identity is the Engine's to stamp; the HTTP adapter no longer needs it.
    let engine = Arc::new(Engine::start(claude_root, codex_root, local_hostname()));
    let (events, _) = broadcast::channel::<Event>(1024);
    spawn_local_events(engine.clone(), events.clone());

    let remote: RemoteSessions = Arc::new(Mutex::new(HashMap::new()));
    let relay_status = Arc::new(RelayStatus::new(relay.is_some()));
    if let Some(relay) = relay {
        spawn_relay(relay, remote.clone(), relay_status.clone(), events.clone());
    }

    Started {
        state: AppState {
            engine,
            events,
            web_dist,
            launcher: Arc::new(TerminalLauncher),
            remote,
            relay_status,
            journal_enabled,
            journal_dir: None,
        },
    }
}

/// Forward local Engine events to the board-facing stream. Relay events use the
/// same board stream, but never enter the local-session Engine.
fn spawn_local_events(engine: Arc<Engine>, events: BoardEvents) {
    tokio::spawn(async move {
        let mut rx = engine.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = events.send(event);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Subscribe to a Relay, retain remote sessions for late HTTP snapshots, and
/// publish its events on the board stream so Board SSE has one event path.
fn spawn_relay(
    relay: RelayConfig,
    remote: RemoteSessions,
    status: Arc<RelayStatus>,
    events: BoardEvents,
) {
    tokio::spawn(async move {
        relay::subscribe(relay.url, relay.token, move |update| match update {
            relay::Update::Connected => {
                status.connected.store(true, Ordering::Relaxed);
                let cleared: Vec<String> =
                    remote.lock().unwrap().drain().map(|(id, _)| id).collect();
                for id in cleared {
                    let _ = events.send(Event::Removed { id });
                }
            }
            relay::Update::Disconnected => {
                status.connected.store(false, Ordering::Relaxed);
            }
            relay::Update::Event(event) => {
                match &event {
                    Event::Upsert(session) => {
                        remote
                            .lock()
                            .unwrap()
                            .insert(session.id.clone(), session.clone());
                    }
                    Event::Removed { id } => {
                        remote.lock().unwrap().remove(id);
                    }
                }
                let _ = events.send(event);
            }
        })
        .await;
    });
}
