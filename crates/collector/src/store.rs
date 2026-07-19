//! The in-memory store of Agent Sessions: discovery, incremental ingest, and the
//! [`Event`]s that flow to connected boards.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};
use tracing::warn;

use crate::model::Session;
use crate::session::FileState;

/// Sessions whose transcript has not been touched within this window are not
/// shown at all on startup (older sessions are out of scope for C1).
pub const DISCOVERY_WINDOW: Duration = Duration::hours(24);

/// A change the store wants pushed to boards. Each carries a full Session so the
/// SSE stream is idempotent — clients upsert by `id`.
#[derive(Debug, Clone)]
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

/// Holds one [`FileState`] per discovered transcript. Not internally synchronized;
/// wrap in a mutex to share between the watcher and the HTTP layer.
#[derive(Default)]
pub struct SessionStore {
    files: HashMap<PathBuf, FileEntry>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan `root` for `<project>/<uuid>.jsonl` transcripts touched within
    /// [`DISCOVERY_WINDOW`] and ingest each fully. Returns the initial snapshot.
    pub fn scan(&mut self, root: &Path, now: DateTime<Utc>) -> Vec<Session> {
        let cutoff = now - DISCOVERY_WINDOW;
        let project_dirs = match fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) => {
                warn!(?root, error = %e, "cannot read projects root");
                return Vec::new();
            }
        };
        for project_dir in project_dirs.flatten() {
            let dir = project_dir.path();
            if !dir.is_dir() {
                continue;
            }
            let files = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    warn!(?dir, error = %e, "cannot read project dir");
                    continue;
                }
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                match file_mtime(&path) {
                    Some(mtime) if mtime >= cutoff => {
                        self.ingest(&path, now);
                    }
                    _ => {}
                }
            }
        }
        self.snapshot(now)
    }

    /// Current sessions, with status recomputed against `now` (so time-based
    /// transitions like Active -> Finished are reflected without a file change).
    pub fn snapshot(&self, now: DateTime<Utc>) -> Vec<Session> {
        self.files
            .values()
            .filter_map(|e| e.state.build(e.mtime, now))
            .collect()
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

        let entry = self
            .files
            .entry(path.to_path_buf())
            .or_insert_with(|| FileEntry {
                state: FileState::default(),
                mtime,
                session: None,
            });

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
