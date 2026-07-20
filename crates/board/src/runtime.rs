//! Wiring the collector to the HTTP layer: initial discovery, the filesystem
//! watcher, and the periodic status refresh.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use collector::{
    Change, ClaudeSource, CodexSource, DiffCache, Event, Session, SessionSource, SessionStore,
    WatchGuard,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::http::AppState;
use crate::open::TerminalLauncher;

/// How often statuses are re-evaluated so time-based transitions (e.g. a quiet
/// session crossing into Finished) reach connected boards without a file change.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Where to subscribe for remote sessions (C7). Absent → local-only, zero-setup
/// solo mode (User Story 1): the board behaves exactly as it did before C7.
pub struct RelayConfig {
    pub url: String,
    pub token: String,
}

/// The board's live Relay-subscription state, surfaced to the UI for the topbar
/// pill. `configured` is fixed for the process; `connected` flips as the board's
/// subscription drops and reconnects.
pub struct RelayStatus {
    pub configured: bool,
    pub connected: AtomicBool,
}

impl RelayStatus {
    fn new(configured: bool) -> Self {
        RelayStatus {
            configured,
            connected: AtomicBool::new(false),
        }
    }
}

/// Remote sessions relayed from other machines, keyed by session id. Held apart
/// from the file-backed local [`SessionStore`] (these have no local transcript to
/// tail); merged with local sessions only at the `/api/sessions` output boundary.
pub type RemoteSessions = Arc<Mutex<HashMap<String, Session>>>;

/// This machine's name, for stamping local sessions (C7). Falls back to `unknown`
/// if the OS hostname cannot be read, so a card is never left unlabelled.
pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Stamp a local session with this machine's name, unless it already carries one.
/// The guard keeps a session that arrives already-tagged (a future remote session
/// relayed from another machine) from being overwritten with the local host.
pub fn stamp_local(session: &mut Session, machine: &str) {
    if session.machine.is_none() {
        session.machine = Some(machine.to_string());
    }
}

/// Stamp an upsert event's session; other events pass through unchanged.
fn stamp_event(event: &mut Event, machine: &str) {
    if let Event::Upsert(session) = event {
        stamp_local(session, machine);
    }
}

/// A running collector: the shared [`AppState`] plus the watcher guard that must
/// be kept alive for filesystem events to keep flowing.
pub struct Started {
    pub state: AppState,
    /// Kept alive for the process lifetime; dropping it stops live updates.
    pub watch_guard: Option<WatchGuard>,
}

/// Scan the Claude Code and (optional) Codex roots, start watching them, and begin
/// the periodic status refresh. A `None` `codex_root` runs Claude-only. A `Some`
/// `relay` also subscribes to a Relay, merging remote sessions into the board (C7);
/// `None` keeps the board local-only, exactly as before C7 (User Story 1).
///
/// Must be called from within a Tokio runtime (it spawns the refresh task).
pub fn init(
    claude_root: PathBuf,
    codex_root: Option<PathBuf>,
    web_dist: Option<PathBuf>,
    relay: Option<RelayConfig>,
) -> Started {
    // Canonicalize so the paths from filesystem events match the keys produced by
    // discovery (on macOS a root may sit under a symlinked tmp / home path, and
    // FSEvents reports the resolved path). A root that does not exist yet keeps its
    // given path — discovery and watching degrade gracefully.
    let claude_root = std::fs::canonicalize(&claude_root).unwrap_or(claude_root);
    let codex_root = codex_root.map(|r| std::fs::canonicalize(&r).unwrap_or(r));

    let mut sources: Vec<Box<dyn SessionSource>> =
        vec![Box::new(ClaudeSource::new(claude_root.clone()))];
    if let Some(root) = codex_root.clone() {
        sources.push(Box::new(CodexSource::new(root)));
    }
    let roots: Vec<PathBuf> = sources.iter().flat_map(|s| s.roots()).collect();

    let store = Arc::new(Mutex::new(SessionStore::new(sources)));
    let (tx, _) = broadcast::channel::<Event>(1024);
    let diff_cache = Arc::new(DiffCache::new());
    // This machine's name, stamped onto every local session so a mixed local +
    // Relay board (C7) labels every card consistently, with no unlabelled "local"
    // special case. Computed once; cheap to share.
    let machine: Arc<str> = Arc::from(local_hostname());

    let count = {
        let mut guard = store.lock().unwrap();
        guard.scan(Utc::now()).len()
    };
    info!(?roots, sessions = count, machine = %machine, "discovered sessions");

    let watch_guard = start_watch(
        &roots,
        store.clone(),
        tx.clone(),
        diff_cache.clone(),
        machine.clone(),
    );
    spawn_refresh(store.clone(), tx.clone(), diff_cache.clone(), machine.clone());

    let remote: RemoteSessions = Arc::new(Mutex::new(HashMap::new()));
    let relay_status = Arc::new(RelayStatus::new(relay.is_some()));
    if let Some(relay) = relay {
        spawn_relay(relay, tx.clone(), remote.clone(), relay_status.clone());
    }

    Started {
        state: AppState {
            store,
            tx,
            web_dist,
            diff_cache,
            launcher: Arc::new(TerminalLauncher),
            machine,
            remote,
            relay_status,
        },
        watch_guard,
    }
}

/// Subscribe to a Relay and merge its remote sessions into the board. Each event is
/// recorded in the shared `remote` map (so a late browser sees remote cards via the
/// `/api/sessions` snapshot) and forwarded onto `tx` (so a live browser sees them
/// stream). A fresh connection resets the remote view: the previous remote cards are
/// cleared and the Relay's snapshot repopulates them, so a Relay restart never
/// leaves a card for a machine that has since gone (self-healing, ADR 0004).
fn spawn_relay(
    relay: RelayConfig,
    tx: broadcast::Sender<Event>,
    remote: RemoteSessions,
    status: Arc<RelayStatus>,
) {
    tokio::spawn(async move {
        relay::subscribe(relay.url, relay.token, move |update| match update {
            relay::Update::Connected => {
                status.connected.store(true, Ordering::Relaxed);
                let cleared: Vec<String> =
                    remote.lock().unwrap().drain().map(|(id, _)| id).collect();
                for id in cleared {
                    let _ = tx.send(Event::Removed { id });
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
                let _ = tx.send(event);
            }
        })
        .await;
    });
}

fn start_watch(
    roots: &[PathBuf],
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
) -> Option<WatchGuard> {
    let result = collector::watch(roots, move |change| {
        let now = Utc::now();
        let event = {
            let mut guard = store.lock().unwrap();
            match change {
                Change::Modified(path) => guard.ingest(&path, now),
                Change::Removed(path) => guard.remove(&path),
            }
        };
        // Runs on the watcher thread, so filling the live `+/-` (a git call, TTL-
        // cached) here keeps it off the async workers.
        if let Some(mut event) = event {
            diff_cache.enrich_event(&mut event);
            stamp_event(&mut event, &machine);
            let _ = tx.send(event);
        }
    });
    match result {
        Ok(guard) => Some(guard),
        Err(e) => {
            warn!(error = %e, "failed to start filesystem watcher; live updates disabled");
            None
        }
    }
}

fn spawn_refresh(
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let mut events = {
                let mut guard = store.lock().unwrap();
                guard.refresh(Utc::now())
            };
            // Enrich off the runtime: the git calls (mostly TTL cache hits) must not
            // stall the async workers for the handful of just-changed sessions.
            let cache = diff_cache.clone();
            let machine = machine.clone();
            let events = tokio::task::spawn_blocking(move || {
                for event in &mut events {
                    cache.enrich_event(event);
                    stamp_event(event, &machine);
                }
                events
            })
            .await
            .expect("diff enrichment does not panic");
            for event in events {
                let _ = tx.send(event);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{local_hostname, stamp_local};
    use chrono::Utc;
    use collector::{Session, Status, Tool};

    fn bare_session() -> Session {
        Session {
            id: "s".into(),
            tool: Tool::Claude,
            project: "p".into(),
            model: None,
            branch: None,
            cwd: None,
            tokens_in: 0,
            tokens_out: 0,
            activity: None,
            last_event_at: Utc::now(),
            status: Status::Active,
            attention_reason: None,
            cost_usd: None,
            diff: None,
            machine: None,
        }
    }

    #[test]
    fn stamps_an_unlabelled_session() {
        let mut s = bare_session();
        stamp_local(&mut s, "loki.local");
        assert_eq!(s.machine.as_deref(), Some("loki.local"));
    }

    #[test]
    fn does_not_overwrite_an_already_tagged_session() {
        // A session relayed from another machine (C7, later) arrives pre-stamped;
        // the local board must not clobber its origin with its own hostname.
        let mut s = bare_session();
        s.machine = Some("forge-desktop".into());
        stamp_local(&mut s, "loki.local");
        assert_eq!(s.machine.as_deref(), Some("forge-desktop"));
    }

    #[test]
    fn local_hostname_is_never_empty() {
        assert!(!local_hostname().is_empty());
    }
}
