//! The in-memory store of Agent Sessions: discovery, incremental ingest, and the
//! [`Event`]s that flow to connected boards.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::liveness::ProcessLiveness;
use crate::model::Session;
use crate::session::FileState;
use crate::source::SessionSource;

/// Sessions whose transcript has not been touched within this window are not
/// shown at all on startup (older sessions are out of scope for C1).
pub const DISCOVERY_WINDOW: Duration = Duration::hours(24);

/// Consecutive probe misses before a matched session is declared Dead. One miss
/// is noise (a probe racing a restart, lsof dropping a pid mid-call); two in a
/// row is a real exit. Ported debounce from the reference implementation.
pub const LIVENESS_MISS_THRESHOLD: u32 = 2;

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
    /// Current process verdict for this transcript ([`apply_liveness`] maintains
    /// it; `Unknown` until a probe has matched or definitively missed it).
    ///
    /// [`apply_liveness`]: SessionStore::apply_liveness
    liveness: ProcessLiveness,
    /// Consecutive probe misses while this entry held its cwd's liveness credit.
    misses: u32,
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

    /// Whether any configured Session Source claims `path`.
    pub fn owns_path(&self, path: &Path) -> bool {
        self.source_for(path).is_some()
    }

    /// Current sessions, with status recomputed against `now` (so time-based
    /// transitions like Active -> Finished are reflected without a file change).
    pub fn snapshot(&self, now: DateTime<Utc>) -> Vec<Session> {
        self.files
            .values()
            .filter_map(|e| e.state.build_with_liveness(e.mtime, now, e.liveness))
            .collect()
    }

    /// Find a live session by its `id`, returning its transcript path alongside
    /// the built Session. Used by the deep-link endpoint to resolve which local
    /// transcript / directory a client-supplied id refers to — the id is the only
    /// client input; the path and `cwd` come from the store, never the caller.
    pub fn find_by_id(&self, id: &str, now: DateTime<Utc>) -> Option<(PathBuf, Session)> {
        self.files.iter().find_map(|(path, entry)| {
            let session = entry
                .state
                .build_with_liveness(entry.mtime, now, entry.liveness)?;
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
                    liveness: ProcessLiveness::Unknown,
                    misses: 0,
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

        let session = entry
            .state
            .build_with_liveness(mtime, now, entry.liveness)?;
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
            if let Some(session) = entry
                .state
                .build_with_liveness(entry.mtime, now, entry.liveness)
            {
                if entry.session.as_ref() != Some(&session) {
                    entry.session = Some(session.clone());
                    events.push(Event::Upsert(session));
                }
            }
        }
        events
    }

    /// Fold one process-liveness probe into per-session verdicts, emitting an
    /// `Upsert` for each session whose status changed as a result.
    ///
    /// `alive_cwds` is the set of directories currently hosting an agent process
    /// (see [`probe_alive_cwds`]). A *failed* probe must not reach here at all —
    /// skipping a tick leaves every verdict as it was, so an lsof timeout can
    /// never mass-finish the board (fail open).
    ///
    /// Per cwd, only the most-recently-touched transcript gets the liveness
    /// credit: a directory holds at most one live agent process, and crediting
    /// every historical transcript there would resurrect finished sessions. All
    /// other entries stay `Unknown` and keep the mtime rule. A credited entry
    /// whose cwd is missing from the probe accrues misses and flips to `Dead`
    /// only at [`LIVENESS_MISS_THRESHOLD`] — the ported anti-flap debounce.
    ///
    /// [`probe_alive_cwds`]: crate::liveness::probe_alive_cwds
    pub fn apply_liveness(
        &mut self,
        alive_cwds: &HashSet<String>,
        now: DateTime<Utc>,
    ) -> Vec<Event> {
        // The one entry per cwd that owns the liveness credit: newest mtime wins.
        let mut credit: HashMap<String, PathBuf> = HashMap::new();
        for (path, entry) in &self.files {
            let Some(cwd) = entry.session.as_ref().and_then(|s| s.cwd.clone()) else {
                continue;
            };
            let cwd = canonical_cwd(&cwd);
            match credit.get(&cwd) {
                Some(cur) if self.files[cur].mtime >= entry.mtime => {}
                _ => {
                    credit.insert(cwd, path.clone());
                }
            }
        }
        let credited: HashMap<PathBuf, String> =
            credit.into_iter().map(|(cwd, path)| (path, cwd)).collect();

        let mut events = Vec::new();
        for (path, entry) in &mut self.files {
            match credited.get(path) {
                Some(cwd) if alive_cwds.contains(cwd) => {
                    entry.misses = 0;
                    entry.liveness = ProcessLiveness::Alive;
                }
                Some(_) => {
                    entry.misses = (entry.misses + 1).min(LIVENESS_MISS_THRESHOLD);
                    if entry.misses >= LIVENESS_MISS_THRESHOLD {
                        entry.liveness = ProcessLiveness::Dead;
                    } else if entry.liveness == ProcessLiveness::Alive {
                        // First miss: keep trusting the last sighting until the
                        // debounce is satisfied.
                    } else {
                        entry.liveness = ProcessLiveness::Unknown;
                    }
                }
                None => {
                    entry.misses = 0;
                    entry.liveness = ProcessLiveness::Unknown;
                }
            }
            if let Some(session) = entry
                .state
                .build_with_liveness(entry.mtime, now, entry.liveness)
            {
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

/// A session cwd normalized for comparison against `lsof` output, which reports
/// symlink-resolved paths (`/tmp` → `/private/tmp` on macOS) while transcripts
/// record the user's spelling. A vanished directory keeps its given spelling.
fn canonical_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    fs::canonicalize(trimmed)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| trimmed.to_string())
}

fn file_mtime(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(system_time_to_utc)
}

fn system_time_to_utc(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::model::Status;
    use crate::source::ClaudeSource;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn assistant_line(id: &str, cwd: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"{id}","timestamp":"2026-07-19T10:00:00Z","cwd":"{cwd}","message":{{"model":"m","usage":{{"input_tokens":1,"output_tokens":1}},"content":[{{"type":"text","text":"hi"}}]}}}}"#
        )
    }

    /// A store over one Claude root with `files` transcripts, each `(name, session
    /// id, cwd)`. Every transcript's ingest time is `now`, so each starts Active.
    fn store_with(root: &Path, files: &[(&str, &str, &str)], now: DateTime<Utc>) -> SessionStore {
        let project = root.join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(root.to_path_buf()))]);
        for (name, id, cwd) in files {
            let path = project.join(name);
            fs::write(&path, format!("{}\n", assistant_line(id, cwd))).unwrap();
            store.ingest(&path, now);
        }
        store
    }

    fn status_of(store: &SessionStore, id: &str, now: DateTime<Utc>) -> Status {
        store
            .snapshot(now)
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .status
    }

    #[test]
    fn dead_process_finishes_a_fresh_session_only_after_two_misses() {
        // The Ctrl-C false positive: transcript fresh, process gone. One miss must
        // not flip the card (anti-flap debounce); the second one does.
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("wd");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.to_str().unwrap().to_string();
        let now = ts("2026-07-19T10:00:30Z");
        let mut store = store_with(dir.path(), &[("a.jsonl", "s1", &cwd)], now);
        assert_eq!(status_of(&store, "s1", now), Status::Active);

        let none = HashSet::new();
        store.apply_liveness(&none, now);
        assert_eq!(status_of(&store, "s1", now), Status::Active); // 1 miss: debounced

        let events = store.apply_liveness(&none, now);
        assert_eq!(status_of(&store, "s1", now), Status::Finished); // 2 misses: dead
        assert!(matches!(&events[..], [Event::Upsert(s)] if s.status == Status::Finished));
    }

    #[test]
    fn alive_process_keeps_a_stale_session_active() {
        // The reverse lie: process alive, transcript quiet past the 15-min window.
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("wd");
        fs::create_dir_all(&cwd).unwrap();
        let cwd_str = cwd.to_str().unwrap().to_string();
        let ingested = ts("2026-07-19T10:00:30Z");
        let mut store = store_with(dir.path(), &[("a.jsonl", "s1", &cwd_str)], ingested);
        // Ingest keeps the real file mtime; pin it to the scenario's timeline.
        for entry in store.files.values_mut() {
            entry.mtime = ingested;
        }

        let later = ts("2026-07-19T10:25:00Z");
        assert_eq!(status_of(&store, "s1", later), Status::Finished); // mtime rule alone

        let alive: HashSet<String> = [fs::canonicalize(&cwd)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()]
        .into();
        store.apply_liveness(&alive, later);
        assert_eq!(status_of(&store, "s1", later), Status::Active);
    }

    #[test]
    fn only_the_newest_transcript_per_cwd_gets_liveness_credit() {
        // Two transcripts in one directory: the live process belongs to the newer
        // one; the older must not be resurrected, and it also must not be declared
        // dead (it is simply unmatched → mtime rule).
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("wd");
        fs::create_dir_all(&cwd).unwrap();
        let cwd_str = cwd.to_str().unwrap().to_string();
        let old = ts("2026-07-19T09:00:00Z");
        let new = ts("2026-07-19T10:00:30Z");
        let mut store = store_with(dir.path(), &[], old);
        let project = dir.path().join("-Users-x-repos-foo");
        fs::write(
            project.join("old.jsonl"),
            format!("{}\n", assistant_line("old", &cwd_str)),
        )
        .unwrap();
        store.ingest(&project.join("old.jsonl"), old);
        // Force the mtimes apart regardless of filesystem timestamp granularity.
        store
            .files
            .get_mut(&project.join("old.jsonl"))
            .unwrap()
            .mtime = old;
        fs::write(
            project.join("new.jsonl"),
            format!("{}\n", assistant_line("new", &cwd_str)),
        )
        .unwrap();
        store.ingest(&project.join("new.jsonl"), new);
        store
            .files
            .get_mut(&project.join("new.jsonl"))
            .unwrap()
            .mtime = new;

        let later = ts("2026-07-19T10:25:00Z");
        let alive: HashSet<String> = [fs::canonicalize(&cwd)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()]
        .into();
        store.apply_liveness(&alive, later);
        assert_eq!(status_of(&store, "new", later), Status::Active); // credited + alive
        assert_eq!(status_of(&store, "old", later), Status::Finished); // mtime rule

        // And when the process dies (twice), only the credited one flips early.
        let none = HashSet::new();
        store.apply_liveness(&none, later);
        store.apply_liveness(&none, later);
        assert_eq!(status_of(&store, "new", later), Status::Finished);
    }

    #[test]
    fn alive_sighting_resets_the_miss_counter() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("wd");
        fs::create_dir_all(&cwd).unwrap();
        let cwd_str = cwd.to_str().unwrap().to_string();
        let now = ts("2026-07-19T10:00:30Z");
        let mut store = store_with(dir.path(), &[("a.jsonl", "s1", &cwd_str)], now);

        let alive: HashSet<String> = [fs::canonicalize(&cwd)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()]
        .into();
        let none = HashSet::new();
        store.apply_liveness(&none, now); // miss #1
        store.apply_liveness(&alive, now); // seen again — counter resets
        store.apply_liveness(&none, now); // miss #1 again, not #2
        assert_eq!(status_of(&store, "s1", now), Status::Active);
    }
}
