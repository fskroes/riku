//! Wiring the collector to the HTTP layer: initial discovery, the filesystem
//! watcher, and the periodic status refresh.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use collector::{Change, Event, SessionStore, WatchGuard};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::http::AppState;

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

/// Scan `root`, start watching it, and begin the periodic status refresh.
///
/// Must be called from within a Tokio runtime (it spawns the refresh task).
pub fn init(root: PathBuf, web_dist: PathBuf) -> Started {
    // Canonicalize so the paths from filesystem events match the keys produced
    // by discovery (on macOS the projects root may sit under a symlinked tmp /
    // home path, and FSEvents reports the resolved path).
    let root = std::fs::canonicalize(&root).unwrap_or(root);

    let store = Arc::new(Mutex::new(SessionStore::new()));
    let (tx, _) = broadcast::channel::<Event>(1024);

    let count = {
        let mut guard = store.lock().unwrap();
        guard.scan(&root, Utc::now()).len()
    };
    info!(?root, sessions = count, "discovered sessions");

    let watch_guard = start_watch(&root, store.clone(), tx.clone());
    spawn_refresh(store.clone(), tx.clone());

    Started {
        state: AppState { store, tx, web_dist },
        watch_guard,
    }
}

fn start_watch(
    root: &std::path::Path,
    store: Arc<Mutex<SessionStore>>,
    tx: broadcast::Sender<Event>,
) -> Option<WatchGuard> {
    let result = collector::watch(root, move |change| {
        let now = Utc::now();
        let event = {
            let mut guard = store.lock().unwrap();
            match change {
                Change::Modified(path) => guard.ingest(&path, now),
                Change::Removed(path) => guard.remove(&path),
            }
        };
        if let Some(event) = event {
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

fn spawn_refresh(store: Arc<Mutex<SessionStore>>, tx: broadcast::Sender<Event>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let events = {
                let mut guard = store.lock().unwrap();
                guard.refresh(Utc::now())
            };
            for event in events {
                let _ = tx.send(event);
            }
        }
    });
}
