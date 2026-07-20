//! Wiring the collector to the HTTP layer: initial discovery, the filesystem
//! watcher, and the periodic status refresh.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use collector::{
    Change, ClaudeSource, CodexSource, Event, SessionSource, SessionStore, WatchGuard,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::diff::DiffCache;
use crate::http::AppState;
use crate::open::TerminalLauncher;

/// How often statuses are re-evaluated so time-based transitions (e.g. a quiet
/// session crossing into Finished) reach connected boards without a file change.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// A running collector: the shared [`AppState`] plus the watcher guard that must
/// be kept alive for filesystem events to keep flowing.
pub struct Started {
    pub state: AppState,
    /// Kept alive for the process lifetime; dropping it stops live updates.
    pub watch_guard: Option<WatchGuard>,
}

/// Scan the Claude Code and (optional) Codex roots, start watching them, and begin
/// the periodic status refresh. A `None` `codex_root` runs Claude-only.
///
/// Must be called from within a Tokio runtime (it spawns the refresh task).
pub fn init(claude_root: PathBuf, codex_root: Option<PathBuf>, web_dist: PathBuf) -> Started {
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

    let count = {
        let mut guard = store.lock().unwrap();
        guard.scan(Utc::now()).len()
    };
    info!(?roots, sessions = count, "discovered sessions");

    let watch_guard = start_watch(&roots, store.clone(), tx.clone(), diff_cache.clone());
    spawn_refresh(store.clone(), tx.clone(), diff_cache.clone());

    Started {
        state: AppState {
            store,
            tx,
            web_dist,
            diff_cache,
            launcher: Arc::new(TerminalLauncher),
        },
        watch_guard,
    }
}

fn start_watch(
    roots: &[PathBuf],
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
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
            let events = tokio::task::spawn_blocking(move || {
                for event in &mut events {
                    cache.enrich_event(event);
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
