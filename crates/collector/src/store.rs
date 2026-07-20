//! The in-memory store of Agent Sessions: discovery, incremental ingest, and the
//! [`Event`]s that flow to connected boards.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::model::Session;
use crate::session::FileState;
use crate::source::SessionSource;

/// Sessions whose transcript has not been touched within this window are not
/// shown at all on startup (older sessions are out of scope for C1).
pub const DISCOVERY_WINDOW: Duration = Duration::hours(24);

/// How deep discovery recurses under a source root. Claude Code nests transcripts
/// two levels (`<project>/<uuid>.jsonl`); Codex four (`YYYY/MM/DD/rollout.jsonl`).
/// A bounded walk covers both without following pathological trees.
const MAX_SCAN_DEPTH: usize = 6;

/// A change the store wants pushed to boards. Each carries a full Session so the
/// SSE stream is idempotent — clients upsert by `id`.
///
/// The size gap between `Upsert` (a whole Session) and `Removed` is deliberate:
/// carrying the full snapshot is the wire contract that makes the stream
/// self-healing (a dropped message or reconnect re-syncs on the next Upsert), and
/// C7 reuses this same `Event` as the Collector→Relay→Board currency. Boxing to
/// even the variants out would trade that clarity for an allocation on the hot
/// path, so the lint is allowed rather than worked around.
///
/// `Serialize`/`Deserialize` (C7): the wire currency for both remote hops. The
/// internally-tagged `type` discriminator rides alongside a flattened `Session`,
/// so an `Upsert` is `{"type":"upsert", <session fields…>}` and a `Removed` is
/// `{"type":"removed","id":…}` — one JSON object per event, NDJSON on the
/// Collector→Relay push and inside the SSE `data:` on the Relay→Board fan-out.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Upsert(Session),
    Removed { id: String },
}

struct FileEntry {
    state: FileState,
    mtime: DateTime<Utc>,
    /// Last Session built for this file, cached to diff against and to know which
    /// id to remove when the file disappears.
    session: Option<Session>,
}

/// Holds one [`FileState`] per discovered transcript, across every configured
/// [`SessionSource`]. Not internally synchronized; wrap in a mutex to share
/// between the watcher and the HTTP layer.
pub struct SessionStore {
    sources: Vec<Box<dyn SessionSource>>,
    files: HashMap<PathBuf, FileEntry>,
}

impl SessionStore {
    /// Build a store over the given sources (e.g. Claude Code + Codex CLI).
    pub fn new(sources: Vec<Box<dyn SessionSource>>) -> Self {
        SessionStore {
            sources,
            files: HashMap::new(),
        }
    }

    /// Scan every source's roots for transcripts touched within
    /// [`DISCOVERY_WINDOW`] and ingest each fully. Returns the initial snapshot.
    pub fn scan(&mut self, now: DateTime<Utc>) -> Vec<Session> {
        let cutoff = now - DISCOVERY_WINDOW;
        // Collect candidate paths first so the per-source immutable borrow does not
        // overlap the mutable `ingest` below.
        let mut candidates: Vec<PathBuf> = Vec::new();
        for source in &self.sources {
            for root in source.roots() {
                let mut found = Vec::new();
                walk(&root, MAX_SCAN_DEPTH, &mut found);
                for path in found {
                    if source.owns(&path) && file_mtime(&path).is_some_and(|m| m >= cutoff) {
                        candidates.push(path);
                    }
                }
            }
        }
        for path in candidates {
            self.ingest(&path, now);
        }
        self.snapshot(now)
    }

    /// The source that owns `path`: the one whose root is an ancestor and whose
    /// [`owns`](SessionSource::owns) accepts the file. Roots are disjoint across
    /// sources, so at most one matches.
    fn source_for(&self, path: &Path) -> Option<&dyn SessionSource> {
        self.sources
            .iter()
            .find(|s| s.roots().iter().any(|r| path.starts_with(r)) && s.owns(path))
            .map(|s| s.as_ref())
    }

    /// Current sessions, with status recomputed against `now` (so time-based
    /// transitions like Active -> Finished are reflected without a file change).
    pub fn snapshot(&self, now: DateTime<Utc>) -> Vec<Session> {
        self.files
            .values()
            .filter_map(|e| e.state.build(e.mtime, now))
            .collect()
    }

    /// Find a live session by its `id`, returning its transcript path alongside
    /// the built Session. Used by the deep-link endpoint to resolve which local
    /// transcript / directory a client-supplied id refers to — the id is the only
    /// client input; the path and `cwd` come from the store, never the caller.
    pub fn find_by_id(&self, id: &str, now: DateTime<Utc>) -> Option<(PathBuf, Session)> {
        self.files.iter().find_map(|(path, entry)| {
            let session = entry.state.build(entry.mtime, now)?;
            (session.id == id).then(|| (path.clone(), session))
        })
    }

    /// Ingest new bytes for a created/modified transcript. Returns an `Upsert`
    /// event iff the resulting Session changed.
    pub fn ingest(&mut self, path: &Path, now: DateTime<Utc>) -> Option<Event> {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let size = meta.len();
        let mtime = meta.modified().ok().map(system_time_to_utc).unwrap_or(now);

        // First sighting: bind this path to the source that owns it. A path no
        // source claims (e.g. a stray file under a root) is ignored.
        if !self.files.contains_key(path) {
            let fold = self.source_for(path)?.new_fold();
            self.files.insert(
                path.to_path_buf(),
                FileEntry {
                    state: FileState::new(fold),
                    mtime,
                    session: None,
                },
            );
        }
        let entry = self.files.get_mut(path).expect("just inserted");

        // Truncation / rewrite: the file is shorter than what we already consumed.
        if size < entry.state.offset() {
            entry.state.reset();
        }
        match read_from(path, entry.state.offset()) {
            Ok(buf) => entry.state.feed(&buf),
            Err(e) => {
                warn!(?path, error = %e, "failed reading transcript");
                return None;
            }
        }
        entry.mtime = mtime;

        let session = entry.state.build(mtime, now)?;
        let changed = entry.session.as_ref() != Some(&session);
        entry.session = Some(session.clone());
        changed.then_some(Event::Upsert(session))
    }

    /// Drop a removed transcript. Returns a `Removed` event if it had a Session.
    pub fn remove(&mut self, path: &Path) -> Option<Event> {
        let entry = self.files.remove(path)?;
        entry.session.map(|s| Event::Removed { id: s.id })
    }

    /// Re-evaluate every session's status against `now`, emitting an `Upsert` for
    /// each one whose projection changed (e.g. it just crossed into Finished).
    pub fn refresh(&mut self, now: DateTime<Utc>) -> Vec<Event> {
        let mut events = Vec::new();
        for entry in self.files.values_mut() {
            if let Some(session) = entry.state.build(entry.mtime, now) {
                if entry.session.as_ref() != Some(&session) {
                    entry.session = Some(session.clone());
                    events.push(Event::Upsert(session));
                }
            }
        }
        events
    }
}

/// Recursively collect files under `dir`, descending at most `depth` levels.
/// Directories that cannot be read are warned about and skipped, so one missing
/// or unreadable subtree never aborts the whole scan.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            warn!(?dir, error = %e, "cannot read source directory");
            return;
        }
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth - 1, out);
        } else {
            out.push(path);
        }
    }
}

/// Read a file from `offset` to EOF.
fn read_from(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn file_mtime(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path).ok()?.modified().ok().map(system_time_to_utc)
}

fn system_time_to_utc(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}
