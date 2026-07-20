//! Live git diff enrichment for session cards (C5).
//!
//! A Session's `+/-` is live working-tree state, not transcript data, so the
//! sessions projection leaves [`Session::diff`] `None` and whichever process
//! owns the repo fills it here: the board for local sessions, and — since C7 — the
//! Collector for the sessions it pushes to the Relay (the repo lives on the
//! Collector's machine, so only it can read the diff). Enrichment is a decoration
//! applied *at the output boundary* (the snapshot response, each outgoing SSE
//! upsert, each pushed event), never stored back in the session store, so it can
//! never perturb the store's change-detection.
//!
//! Shelling out to `git` per card per event would be wasteful, so results are
//! cached per directory with a short TTL: a fast-moving agent whose transcript
//! changes every few seconds recomputes its diff at most once per [`TTL`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::git::diff_stat;
use crate::model::{DiffStat, Session};
use crate::store::Event;

/// How long a computed diff is reused before the next request recomputes it.
const TTL: Duration = Duration::from_secs(10);

struct Entry {
    stat: Option<DiffStat>,
    at: Instant,
}

/// A per-directory cache of git diff stats, safe to share across the HTTP handlers,
/// the filesystem watcher, the refresh task, and the Collector's push loop.
#[derive(Default)]
pub struct DiffCache {
    inner: Mutex<HashMap<PathBuf, Entry>>,
}

impl DiffCache {
    pub fn new() -> Self {
        DiffCache::default()
    }

    /// The diff stat for `dir`, recomputing via `git` when absent or older than
    /// [`TTL`]. `None` when `dir` is not a git repo (or `git` is unavailable).
    pub fn get(&self, dir: &Path) -> Option<DiffStat> {
        {
            let cache = self.inner.lock().unwrap();
            if let Some(e) = cache.get(dir) {
                if e.at.elapsed() < TTL {
                    return e.stat;
                }
            }
        }
        // Compute outside the lock: `git` can take tens of ms, and one slow repo
        // must not block cache reads for every other card.
        let stat = diff_stat(dir);
        let mut cache = self.inner.lock().unwrap();
        cache.insert(
            dir.to_path_buf(),
            Entry {
                stat,
                at: Instant::now(),
            },
        );
        stat
    }

    /// Attach the live diff to a session (no-op if it has no `cwd`).
    pub fn enrich(&self, session: &mut Session) {
        if let Some(cwd) = session.cwd.as_deref() {
            session.diff = self.get(Path::new(cwd));
        }
    }

    /// Attach the live diff to an upsert event's session; other events pass through.
    pub fn enrich_event(&self, event: &mut Event) {
        if let Event::Upsert(session) = event {
            self.enrich(session);
        }
    }
}
