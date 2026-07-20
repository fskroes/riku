//! Recursive filesystem watching of the projects root, debounced per file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use tracing::warn;

/// Coalesce bursts of change events for the same file within this window.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// A debounced change to a single transcript.
#[derive(Debug, Clone)]
pub enum Change {
    /// The file was created or modified — re-ingest from its offset.
    Modified(PathBuf),
    /// The file no longer exists — drop its Session.
    Removed(PathBuf),
}

/// Keeps the watcher and debounce thread alive; dropping it stops watching.
pub struct WatchGuard {
    _watcher: notify::RecommendedWatcher,
    _handle: thread::JoinHandle<()>,
}

/// Watch each of `roots` recursively for `*.jsonl` changes, invoking `on_change`
/// once per file per debounce window. `on_change` runs on a dedicated thread.
///
/// A root that cannot be watched (e.g. a missing `~/.codex/sessions`) is warned
/// about and skipped rather than failing the whole watcher, so one absent source
/// never takes the others down. Constructing the watcher itself can still fail.
pub fn watch<F>(roots: &[PathBuf], mut on_change: F) -> notify::Result<WatchGuard>
where
    F: FnMut(Change) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<PathBuf>();

    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                for path in event.paths {
                    if is_transcript(&path) {
                        let _ = tx.send(path);
                    }
                }
            }
            Err(e) => warn!(error = %e, "watch error"),
        })?;
    for root in roots {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            warn!(?root, error = %e, "cannot watch source root; skipping");
        }
    }

    let handle = thread::spawn(move || debounce_loop(rx, &mut on_change));

    Ok(WatchGuard {
        _watcher: watcher,
        _handle: handle,
    })
}

fn debounce_loop<F: FnMut(Change)>(rx: mpsc::Receiver<PathBuf>, on_change: &mut F) {
    // path -> last time we saw a raw event for it.
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(path) => {
                pending.insert(path, Instant::now());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Watcher dropped: no more events will ever arrive.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                flush(&mut pending, on_change, true);
                break;
            }
        }
        flush(&mut pending, on_change, false);
    }
}

/// Emit `Change`s for files whose debounce window has elapsed (or all of them
/// when `force`). A file that no longer exists becomes `Removed`.
fn flush<F: FnMut(Change)>(
    pending: &mut HashMap<PathBuf, Instant>,
    on_change: &mut F,
    force: bool,
) {
    let ready: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, seen)| force || seen.elapsed() >= DEBOUNCE)
        .map(|(p, _)| p.clone())
        .collect();
    for path in ready {
        pending.remove(&path);
        let change = if path.exists() {
            Change::Modified(path)
        } else {
            Change::Removed(path)
        };
        on_change(change);
    }
}

fn is_transcript(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}
