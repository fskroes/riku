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

use crate::fold::{assemble, merge_roster, Folded, Projection};
use crate::liveness::ProcessLiveness;
use crate::model::{Session, SubAgent};
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

/// Roster rows contributed by discovered Sub-agent files, indexed by the id of the
/// **root** Agent Session each belongs to — the only node in a spawn tree that is a
/// card, however deep the Sub-agent was spawned.
type Rosters = HashMap<String, Vec<SubAgent>>;

struct FileEntry {
    state: FileState,
    mtime: DateTime<Utc>,
    /// Last Session built for this file, cached to diff against and to know which
    /// id to remove when the file disappears. Always `None` for a Sub-agent's file:
    /// a Sub-agent is folded in full and is never a card.
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
            self.feed(&path, now);
        }
        // Join after every file is folded, rather than per ingest — which would
        // re-index every Sub-agent for every transcript found. `refresh` is called
        // for its caching side effect (its events are the whole corpus and nobody
        // has subscribed yet); caching here is what stops the first live refresh
        // from re-emitting every card.
        self.refresh(now);
        self.snapshot(now)
    }

    /// Every discovered Sub-agent's roster row, indexed by its root Agent Session.
    ///
    /// This is the cross-file join the store owns. A Sub-agent's spend is in its own
    /// file and what it was sent to do is in a sidecar beside that file, while the
    /// spawn that created it is in a third — its root's transcript. Indexing by root
    /// here, and merging in [`build`], is what lets `assemble` stay the one pure
    /// projection-to-card seam and gain an input rather than a second job.
    ///
    /// A Sub-agent whose root the source could not resolve is **held out** rather
    /// than attached to a guess: it waits, invisible, until its root is discovered.
    ///
    /// **Known bound, recorded rather than fixed.** A Sub-agent's file is filtered by
    /// [`DISCOVERY_WINDOW`] on its *own* mtime, like any other transcript, so a
    /// Sub-agent that went quiet more than a day ago does not appear on the roster of
    /// a parent that is still fresh. The parent's own spawn record survives — the row
    /// is there, with its Errand — but it reports no spend, and the card's headline
    /// tokens and cost under-report by that Sub-agent's share with nothing on screen
    /// to say so. The roster reflects what discovery found. Discovering Sub-agent
    /// files by their *root's* freshness instead of their own would fix it and is a
    /// change to discovery, not to this join.
    fn rosters(&self) -> Rosters {
        let mut rosters = Rosters::new();
        for entry in self.files.values() {
            let Some(Folded::SubAgent(sub)) = entry.state.folded() else {
                continue;
            };
            let Some(root) = sub.root_session_id.clone() else {
                continue;
            };
            rosters.entry(root).or_default().push(sub.roster_entry());
        }
        rosters
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
    /// transitions like Active -> Finished are reflected without a file change) and
    /// every Sub-agent joined onto its root's roster.
    pub fn snapshot(&self, now: DateTime<Utc>) -> Vec<Session> {
        let rosters = self.rosters();
        self.files
            .values()
            .filter_map(|e| build(e, &rosters, now))
            .collect()
    }

    /// Find a live session by its `id`, returning its transcript path alongside
    /// the built Session. Used by the deep-link endpoint to resolve which local
    /// transcript / directory a client-supplied id refers to — the id is the only
    /// client input; the path and `cwd` come from the store, never the caller.
    ///
    /// A Sub-agent is never returned: it is not a card, so nothing can be opened or
    /// resumed at it. Its roster is carried on the session this *does* return.
    pub fn find_by_id(&self, id: &str, now: DateTime<Utc>) -> Option<(PathBuf, Session)> {
        let rosters = self.rosters();
        self.files.iter().find_map(|(path, entry)| {
            let session = build(entry, &rosters, now)?;
            (session.id == id).then(|| (path.clone(), session))
        })
    }

    /// Read a transcript's new bytes into its fold, creating the entry on first
    /// sighting. `None` for a path no source claims, or one that cannot be read.
    fn feed(&mut self, path: &Path, now: DateTime<Utc>) -> Option<()> {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let size = meta.len();
        let mtime = meta.modified().ok().map(system_time_to_utc).unwrap_or(now);

        // First sighting: bind this path to the source that owns it. A path no
        // source claims (e.g. a stray file under a root) is ignored.
        if !self.files.contains_key(path) {
            let fold = self.source_for(path)?.new_fold(path);
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
        Some(())
    }

    /// Ingest new bytes for a created/modified transcript. Returns an `Upsert`
    /// event iff the resulting Session changed.
    ///
    /// A Sub-agent's file emits an update for **its root's** session and never one
    /// of its own: what changed is a row on the root's roster. If the root has not
    /// been discovered, nothing is emitted and the Sub-agent waits — held out of
    /// every roster rather than attached to a guess.
    pub fn ingest(&mut self, path: &Path, now: DateTime<Utc>) -> Option<Event> {
        self.feed(path, now)?;
        match self.files.get(path)?.state.folded()? {
            Folded::SubAgent(sub) => self.rebuild_session(&sub.root_session_id?, now),
            Folded::AgentSession(_) => self.rebuild(&path.to_path_buf(), now),
        }
    }

    /// Drop a removed transcript. Returns a `Removed` event if it had a Session.
    ///
    /// A Sub-agent's file never produces a removal of its own — it was never a card.
    /// Its disappearance is a row leaving its root's roster, so the root is re-emitted.
    pub fn remove(&mut self, path: &Path, now: DateTime<Utc>) -> Option<Event> {
        let entry = self.files.remove(path)?;
        if let Some(Folded::SubAgent(sub)) = entry.state.folded() {
            return self.rebuild_session(&sub.root_session_id?, now);
        }
        entry.session.map(|s| Event::Removed { id: s.id })
    }

    /// Rebuild one file's card with its Sub-agents joined on, cache it, and emit an
    /// `Upsert` iff it changed.
    fn rebuild(&mut self, path: &PathBuf, now: DateTime<Utc>) -> Option<Event> {
        let rosters = self.rosters();
        let entry = self.files.get_mut(path)?;
        let session = build(entry, &rosters, now)?;
        let changed = entry.session.as_ref() != Some(&session);
        entry.session = Some(session.clone());
        changed.then_some(Event::Upsert(session))
    }

    /// As [`rebuild`](Self::rebuild), for the transcript carrying Agent Session `id`.
    fn rebuild_session(&mut self, id: &str, now: DateTime<Utc>) -> Option<Event> {
        let path = self.files.iter().find_map(|(path, entry)| {
            matches!(entry.state.folded(), Some(Folded::AgentSession(p)) if p.id == id)
                .then(|| path.clone())
        })?;
        self.rebuild(&path, now)
    }

    /// Re-evaluate every session's status against `now`, emitting an `Upsert` for
    /// each one whose projection changed (e.g. it just crossed into Finished).
    pub fn refresh(&mut self, now: DateTime<Utc>) -> Vec<Event> {
        let rosters = self.rosters();
        let mut events = Vec::new();
        for entry in self.files.values_mut() {
            if let Some(session) = build(entry, &rosters, now) {
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
    /// **Sub-agent files never enter that contest**, and cannot: the credit is read
    /// off `entry.session`, which a Sub-agent's file never has, because a Sub-agent
    /// is not a card. That is structural rather than a filter someone must remember,
    /// and it matters — a Sub-agent shares its parent's working directory *exactly*
    /// and is usually more recently active, so letting one compete would drop the
    /// **parent** to `Unknown` and back onto the Staleness heuristic, precisely for
    /// the sessions doing the most work (ADR 0014). The liveness probe already votes
    /// this way one layer down, stripping Sub-agent worktree processes from the pool.
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

        let rosters = self.rosters();
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
            if let Some(session) = build(entry, &rosters, now) {
                if entry.session.as_ref() != Some(&session) {
                    entry.session = Some(session.clone());
                    events.push(Event::Upsert(session));
                }
            }
        }
        events
    }
}

/// One file's card, with the rows its Sub-agents contributed joined onto the spawns
/// its own transcript recorded.
///
/// `None` for a fold that has no identity yet, and for a Sub-agent's own file — the
/// one place "never a card" is enforced, by type rather than by convention.
fn build(entry: &FileEntry, rosters: &Rosters, now: DateTime<Utc>) -> Option<Session> {
    let Folded::AgentSession(p) = entry.state.folded()? else {
        return None;
    };
    let contributions = rosters.get(&p.id).cloned().unwrap_or_default();
    let sub_agent_roster = merge_roster(p.sub_agent_roster, contributions);
    Some(assemble(
        Projection {
            sub_agent_roster,
            ..p
        },
        entry.mtime,
        now,
        entry.liveness,
    ))
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

    // --- the cross-file Sub-agent join -----------------------------------------
    //
    // A Sub-agent's spend is in its own file, what it was sent to do is in a sidecar
    // beside that file, and the spawn that created it is in a third — its root's
    // transcript. Only the store sees all three.

    /// A main-chain `Agent` tool-use: the parent recording that it sent a Sub-agent
    /// out, and what for.
    fn agent_spawn_line(id: &str, cwd: &str, tuid: &str, errand: &str) -> String {
        serde_json::json!({
            "type": "assistant", "sessionId": id, "cwd": cwd,
            "timestamp": "2026-07-19T10:00:00Z",
            "message": {
                "model": "claude-opus-4-8", "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use", "id": tuid, "name": "Agent",
                    "input": { "description": errand, "subagent_type": "Explore" }
                }]
            }
        })
        .to_string()
    }

    /// The sidecar Claude writes beside a Sub-agent's transcript **at spawn**, in
    /// the four-field shape 70 of 70 real ones carry.
    fn meta(errand: &str, tuid: &str, depth: u32) -> serde_json::Value {
        serde_json::json!({
            "agentType": "Explore", "description": errand,
            "toolUseId": tuid, "spawnDepth": depth,
        })
    }

    /// Write a Sub-agent's transcript and its spawn-time sidecar under `root`'s
    /// `subagents/` directory, where Claude Code puts them.
    fn write_sub_agent(
        project: &Path,
        root_id: &str,
        agent_id: &str,
        meta: serde_json::Value,
        cwd: &str,
        tin: u64,
    ) -> PathBuf {
        let dir = project.join(root_id).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("agent-{agent_id}.meta.json")),
            meta.to_string(),
        )
        .unwrap();
        let path = dir.join(format!("agent-{agent_id}.jsonl"));
        let line = serde_json::json!({
            "type": "assistant", "isSidechain": true, "agentId": agent_id,
            // Claude stamps the *root* session's id on every child entry, at any depth.
            "sessionId": root_id, "cwd": cwd, "gitBranch": "main",
            "timestamp": "2026-07-19T10:00:10Z",
            "message": {
                "model": "claude-haiku-4-5",
                "usage": { "input_tokens": tin, "output_tokens": 10 },
                "content": [{ "type": "text", "text": "sub work" }]
            }
        });
        fs::write(&path, format!("{line}\n")).unwrap();
        path
    }

    #[test]
    fn a_sub_agent_file_joins_its_roots_roster_and_is_never_a_card_itself() {
        // The union: the parent's spawn says what the Sub-agent was for, the child's
        // own file says what it spent, and the sidecar's tool-use id makes them one
        // row. The Sub-agent never appears as a session of its own, and is never
        // returned by an id lookup.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        let project = dir.path().join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(dir.path().into()))]);

        let parent = project.join("root-1.jsonl");
        fs::write(
            &parent,
            format!(
                "{}\n",
                agent_spawn_line("root-1", "/a/foo", "toolu_a", "map the parser")
            ),
        )
        .unwrap();
        store.ingest(&parent, now);
        let child = write_sub_agent(
            &project,
            "root-1",
            "a1b2c3",
            meta("map the parser", "toolu_a", 1),
            "/a/foo",
            900,
        );

        // Ingesting the child emits an update for its **root**, never for itself.
        let event = store.ingest(&child, now).expect("the root is re-emitted");
        let Event::Upsert(card) = event else {
            panic!("a Sub-agent's file never produces a removal");
        };
        assert_eq!(card.id, "root-1");
        assert_eq!(card.sub_agent_roster.len(), 1);
        let row = &card.sub_agent_roster[0];
        assert_eq!(row.id, "a1b2c3"); // the child's own id, once it is known
        assert_eq!(row.errand.as_deref(), Some("map the parser"));
        assert_eq!(row.tokens_in, 900);
        assert_eq!(row.depth, 1);
        assert_eq!(row.model.as_deref(), Some("claude-haiku-4-5"));

        // Every build path joins, and none of them yields the Sub-agent as a card.
        let snapshot = store.snapshot(now);
        assert_eq!(
            snapshot.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["root-1"]
        );
        assert_eq!(snapshot[0].sub_agent_roster.len(), 1);
        assert_eq!(
            store
                .find_by_id("root-1", now)
                .unwrap()
                .1
                .sub_agent_roster
                .len(),
            1
        );
        assert!(store.find_by_id("a1b2c3", now).is_none());
        assert!(store.refresh(now).is_empty(), "the join is already cached");

        // And its disappearance re-emits the root rather than removing anything.
        let Some(Event::Upsert(card)) = store.remove(&child, now) else {
            panic!("a Sub-agent leaving re-emits its root");
        };
        assert_eq!(card.id, "root-1");
        assert_eq!(card.sub_agent_roster.len(), 1); // the spawn record remains
        assert_eq!(card.sub_agent_roster[0].tokens_in, 0);
    }

    #[test]
    fn a_sub_agent_spawned_by_a_sub_agent_lands_on_the_roots_roster() {
        // Attachment is to the root — the only node that is a card. A depth-2 child
        // sits beside its depth-1 spawner in the same flat directory, and its spawn
        // was recorded in a child transcript, so the parent's side of the union has
        // no record of it at all. It is a row on the child file's word alone.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        let project = dir.path().join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        let parent = project.join("root-1.jsonl");
        fs::write(&parent, format!("{}\n", assistant_line("root-1", "/a/foo"))).unwrap();
        write_sub_agent(
            &project,
            "root-1",
            "a-deep",
            meta("research the API", "toolu_nested", 2),
            "/a/foo",
            400,
        );

        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(dir.path().into()))]);
        let cards = store.scan(now);
        assert_eq!(cards.len(), 1, "only the root is a card: {cards:?}");
        assert_eq!(cards[0].id, "root-1");
        assert_eq!(cards[0].sub_agent_roster.len(), 1);
        assert_eq!(cards[0].sub_agent_roster[0].depth, 2);
        assert_eq!(
            cards[0].sub_agent_roster[0].errand.as_deref(),
            Some("research the API")
        );
    }

    #[test]
    fn a_sub_agent_whose_root_is_undiscovered_is_held_out_of_every_roster() {
        // The roster reflects what discovery found. A Sub-agent whose root has not
        // been discovered waits — attached to nothing rather than to a guess — and
        // its ingest emits no event at all.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        let project = dir.path().join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        // A different session's transcript; the orphan's root is never written.
        let other = project.join("other.jsonl");
        fs::write(&other, format!("{}\n", assistant_line("other", "/a/foo"))).unwrap();
        let orphan = write_sub_agent(
            &project,
            "root-missing",
            "a-orphan",
            meta("nobody's errand", "toolu_x", 1),
            "/a/foo",
            100,
        );

        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(dir.path().into()))]);
        store.scan(now);
        assert!(store.ingest(&orphan, now).is_none(), "nothing to emit yet");
        let cards = store.snapshot(now);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "other");
        assert!(cards[0].sub_agent_roster.is_empty());
    }

    #[test]
    fn a_sub_agent_quiet_past_the_discovery_window_leaves_its_spend_off_the_card() {
        // The known bound, asserted rather than merely written down. Discovery filters
        // a Sub-agent's file on its *own* mtime, so a fresh parent with a long-quiet
        // Sub-agent keeps the row (the parent's spawn record survives, Errand and all)
        // but loses its spend — the card's tokens under-report with nothing on screen
        // to say so. Fixing it means discovering child files by their root's
        // freshness; this pins today's behaviour so the change is visible when it comes.
        let dir = tempfile::tempdir().unwrap();
        // Real wall-clock, because the cutoff is compared against real file mtimes.
        let now = Utc::now();
        let project = dir.path().join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        let parent = project.join("root-1.jsonl");
        fs::write(
            &parent,
            format!(
                "{}\n",
                agent_spawn_line("root-1", "/a/foo", "toolu_a", "map the parser")
            ),
        )
        .unwrap();
        let child = write_sub_agent(
            &project,
            "root-1",
            "a1b2c3",
            meta("map the parser", "toolu_a", 1),
            "/a/foo",
            900,
        );
        // The child went quiet two days ago; the parent is fresh.
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        fs::File::options()
            .write(true)
            .open(&child)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(long_ago))
            .unwrap();

        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(dir.path().into()))]);
        let cards = store.scan(now);
        assert_eq!(cards.len(), 1);
        // The row is there, and says what the Sub-agent was sent to do…
        assert_eq!(cards[0].sub_agent_roster.len(), 1);
        assert_eq!(
            cards[0].sub_agent_roster[0].errand.as_deref(),
            Some("map the parser")
        );
        // …but its spend is not, and neither is it in the headline total.
        assert_eq!(cards[0].sub_agent_roster[0].tokens_in, 0);
        assert_eq!(cards[0].tokens_in, 0);
    }

    #[test]
    fn a_parent_keeps_its_liveness_credit_when_a_sub_agent_shares_its_cwd() {
        // A Sub-agent shares its parent's working directory *exactly* and is usually
        // more recently active. If it could take the per-cwd credit, the parent would
        // fall back to the Staleness heuristic — precisely for the sessions doing the
        // most work. It cannot: a Sub-agent is not a card, so it never competes.
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("wd");
        fs::create_dir_all(&cwd).unwrap();
        let cwd_str = cwd.to_str().unwrap().to_string();
        let ingested = ts("2026-07-19T10:00:30Z");
        let project = dir.path().join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        let parent = project.join("root-1.jsonl");
        fs::write(&parent, format!("{}\n", assistant_line("root-1", &cwd_str))).unwrap();
        write_sub_agent(
            &project,
            "root-1",
            "a1b2c3",
            meta("grind on it", "toolu_a", 1),
            &cwd_str,
            900,
        );

        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(dir.path().into()))]);
        store.scan(ingested);
        // Pin both files' mtimes apart the wrong way round: the child is newer, as it
        // is in practice, so a contest it could enter is one the parent would lose.
        for (path, entry) in store.files.iter_mut() {
            entry.mtime = if path == &parent {
                ingested
            } else {
                ts("2026-07-19T10:20:00Z")
            };
        }

        let later = ts("2026-07-19T10:25:00Z");
        let alive: HashSet<String> = [fs::canonicalize(&cwd)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()]
        .into();
        store.apply_liveness(&alive, later);
        assert_eq!(status_of(&store, "root-1", later), Status::Active);
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
