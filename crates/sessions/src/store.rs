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

use crate::fold::{assemble, merge_roster, Attachment, Folded, Projection, SubAgentProjection};
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

/// How many links of a spawn chain the walk to the root will follow before giving
/// up. The deepest chain in the observed corpus is 3, so this is not a cap on
/// nesting; it is what guarantees a chain that loops back on itself — which no
/// honest source writes, and only a file could claim — ends the walk rather than the
/// process.
const MAX_SPAWN_CHAIN: usize = 16;

/// Who is a card and what every Sub-agent hangs from, indexed so that each hop of a
/// walk up a spawn chain is a lookup rather than another pass over the file map.
///
/// The store is the only place this can be built: a Sub-agent's chain runs through
/// files it has no knowledge of, and its root may be a session discovered before or
/// after it. It is rebuilt per join rather than cached, so it cannot go stale against
/// the folds — the folds move on every ingested byte.
#[derive(Default)]
struct SpawnTree {
    /// Every id that is an Agent Session — the ids a chain can end at.
    sessions: HashSet<String>,
    /// What each discovered Sub-agent hangs from, by that Sub-agent's own id.
    attachments: HashMap<String, Attachment>,
}

impl SpawnTree {
    /// The root Agent Session an attachment leads to, or `None` when the chain cannot
    /// be resolved — an unknown id, an unstated attachment, or (defensively) a loop.
    ///
    /// An unresolved chain means the Sub-agent is **held out** of every roster rather
    /// than attached to a guess. The commonest honest cause is a root that discovery
    /// has not reached: it falls outside the discovery window, or its file simply has
    /// not been sighted yet. Both fix themselves the moment the root appears, which is
    /// why holding out costs nothing but the guess.
    ///
    /// [`Attachment::Root`] resolves to itself without consulting the tree at all: the
    /// source named the root, and whether that root has been discovered is the caller's
    /// question, answered by there being no card to put the row on.
    fn root_of(&self, attachment: Option<&Attachment>) -> Option<String> {
        let mut at = attachment?;
        for _ in 0..MAX_SPAWN_CHAIN {
            match at {
                Attachment::Root(id) => return Some(id.clone()),
                Attachment::Spawner(id) if self.sessions.contains(id) => return Some(id.clone()),
                // The spawner is itself a Sub-agent: keep climbing towards the card.
                Attachment::Spawner(id) => at = self.attachments.get(id)?,
            }
        }
        None
    }
}

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
    /// A Sub-agent whose root cannot be resolved is **held out** rather than attached
    /// to a guess: it waits, invisible, until its root is discovered.
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
        let tree = self.spawn_tree();
        let mut rosters = Rosters::new();
        for entry in self.files.values() {
            let Some(Folded::SubAgent(sub)) = entry.state.folded() else {
                continue;
            };
            let Some(root) = tree.root_of(sub.attachment.as_ref()) else {
                continue;
            };
            rosters.entry(root).or_default().push(sub.roster_entry());
        }
        rosters
    }

    /// Who is a card and what every Sub-agent hangs from, as the folds currently
    /// state it.
    fn spawn_tree(&self) -> SpawnTree {
        let mut tree = SpawnTree::default();
        for entry in self.files.values() {
            match entry.state.folded() {
                Some(Folded::AgentSession(p)) => {
                    tree.sessions.insert(p.id);
                }
                Some(Folded::SubAgent(sub)) => {
                    if let Some(attachment) = sub.attachment {
                        tree.attachments.insert(sub.id, attachment);
                    }
                }
                None => {}
            }
        }
        tree
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
    /// of its own: what changed is a row on the root's roster. If the chain to that
    /// root cannot be resolved, nothing is emitted and the Sub-agent waits — held out
    /// of every roster rather than attached to a guess.
    pub fn ingest(&mut self, path: &Path, now: DateTime<Utc>) -> Option<Event> {
        self.feed(path, now)?;
        match self.files.get(path)?.state.folded()? {
            Folded::SubAgent(sub) => self.rebuild_root_of(&sub, now),
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
            // Resolved *after* the removal, which is what makes the rebuild complete: a
            // chain that ran through this Sub-agent is now broken, so a nested child it
            // spawned is held out on the same rule as any other unresolvable chain, and
            // the one rebuild below drops both rows rather than leaving the grandchild's
            // behind. This Sub-agent's own attachment still resolves — it is read from
            // the removed projection, not from the map.
            return self.rebuild_root_of(&sub, now);
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

    /// As [`rebuild`](Self::rebuild), for the card `sub` belongs to — the root its
    /// chain climbs to. `None`, and so no event, when that chain cannot be resolved:
    /// the Sub-agent is on no roster, so nothing changed for anyone.
    fn rebuild_root_of(&mut self, sub: &SubAgentProjection, now: DateTime<Utc>) -> Option<Event> {
        let root = self.spawn_tree().root_of(sub.attachment.as_ref())?;
        self.rebuild_session(&root, now)
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

    // --- the Codex walk up the spawn chain ---------------------------------------
    //
    // Codex is the mirror image of Claude: it names the *immediate* spawner on every
    // subagent rollout and never the root, so attachment costs a walk that only the
    // store can make — a chain runs through files no single fold knows about.

    /// A Codex rollout: `session_meta` + one cumulative `token_count`. `parent` makes
    /// it a subagent rollout spawned by that thread; `None` makes it an Agent Session.
    fn rollout_lines(id: &str, parent: Option<(&str, u32)>, cwd: &str, tin: u64) -> Vec<String> {
        let mut meta = serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": { "id": id, "cwd": cwd, "git": { "branch": "main" } }
        });
        if let Some((parent, depth)) = parent {
            let payload = meta["payload"].as_object_mut().unwrap();
            payload.insert("thread_source".into(), "subagent".into());
            payload.insert("parent_thread_id".into(), parent.into());
            payload.insert("agent_nickname".into(), "Dirac".into());
            payload.insert(
                "source".into(),
                serde_json::json!({ "subagent": { "thread_spawn": { "depth": depth } } }),
            );
        }
        vec![
            meta.to_string(),
            serde_json::json!({
                "timestamp": "2026-07-19T10:00:01Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.6-sol", "cwd": cwd }
            })
            .to_string(),
            serde_json::json!({
                "timestamp": "2026-07-19T10:00:02Z",
                "type": "event_msg",
                "payload": { "type": "token_count", "info": {
                    "total_token_usage": { "input_tokens": tin, "output_tokens": 10 }
                }}
            })
            .to_string(),
        ]
    }

    /// Write a Codex rollout under a date-nested directory and return its path.
    fn write_codex(root: &Path, id: &str, lines: &[String]) -> PathBuf {
        let dir = root.join("2026/07/19");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-07-19T10-00-00-{id}.jsonl"));
        let mut body = lines.join("\n");
        body.push('\n');
        fs::write(&path, body).unwrap();
        path
    }

    /// Append one committed line to a transcript, as a live agent would.
    fn append_line(path: &Path, line: &str) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn codex_store(root: &Path) -> SessionStore {
        SessionStore::new(vec![Box::new(crate::source::CodexSource::new(
            root.to_path_buf(),
        ))])
    }

    #[test]
    fn a_codex_sub_agent_lands_on_its_spawners_card_and_is_never_one_itself() {
        // The whole of the Codex side in one: the child's rollout is the only file
        // that says anything about it, and what it says is a row on the parent's card.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        write_codex(
            dir.path(),
            "root-1",
            &rollout_lines("root-1", None, "/a/foo", 500),
        );
        let child = write_codex(
            dir.path(),
            "sub-1",
            &rollout_lines("sub-1", Some(("root-1", 1)), "/a/foo", 900),
        );

        let mut store = codex_store(dir.path());
        let cards = store.scan(now);
        assert_eq!(
            cards.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["root-1"],
            "only the spawner is a card"
        );
        let row = &cards[0].sub_agent_roster[0];
        assert_eq!(row.id, "sub-1");
        assert_eq!(row.depth, 1);
        assert_eq!(row.errand, None, "no nickname stands in for an Errand");
        assert_eq!(row.tokens_in, 900);
        // And its spend reaches the headline totals — the number that was 0.
        assert_eq!(cards[0].tokens_in, 1400);

        // Never a lookup target, and its own ingest re-emits the parent instead.
        assert!(store.find_by_id("sub-1", now).is_none());
        assert!(store.find_by_id("root-1", now).is_some());
        // Nor does it take the parent's Process Liveness credit, though it shares the
        // working directory exactly and is the more recently active of the two: the
        // credit is read off a built card, and a Sub-agent never has one.
        for (path, entry) in store.files.iter_mut() {
            entry.mtime = if path == &child {
                now
            } else {
                ts("2026-07-19T09:00:00Z")
            };
        }
        store.apply_liveness(&HashSet::new(), now);
        store.apply_liveness(&HashSet::new(), now);
        assert_eq!(
            status_of(&store, "root-1", now),
            Status::Finished,
            "the parent holds the credit, so its process dying is what decides it"
        );
        append_line(
            &child,
            &rollout_lines("sub-1", Some(("root-1", 1)), "/a/foo", 1500)[2],
        );
        let Some(Event::Upsert(card)) = store.ingest(&child, now) else {
            panic!("a Sub-agent's file re-emits its root");
        };
        assert_eq!(card.id, "root-1");
        assert_eq!(card.sub_agent_roster[0].tokens_in, 1500);
    }

    #[test]
    fn a_codex_sub_agent_spawned_by_a_sub_agent_lands_on_the_root() {
        // Attachment is to the root — the only node that is a card — so the walk keeps
        // climbing while the spawner is itself a Sub-agent (4 of 79 observed rollouts).
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        write_codex(
            dir.path(),
            "root-1",
            &rollout_lines("root-1", None, "/a/foo", 100),
        );
        write_codex(
            dir.path(),
            "sub-1",
            &rollout_lines("sub-1", Some(("root-1", 1)), "/a/foo", 200),
        );
        write_codex(
            dir.path(),
            "sub-2",
            &rollout_lines("sub-2", Some(("sub-1", 2)), "/a/foo", 400),
        );

        let cards = codex_store(dir.path()).scan(now);
        assert_eq!(cards.len(), 1);
        let mut rows: Vec<_> = cards[0]
            .sub_agent_roster
            .iter()
            .map(|s| (s.id.as_str(), s.depth))
            .collect();
        rows.sort();
        assert_eq!(rows, vec![("sub-1", 1), ("sub-2", 2)]);
        assert_eq!(cards[0].tokens_in, 700, "the whole tree's spend");
    }

    #[test]
    fn a_codex_chain_that_cannot_be_resolved_is_held_out_of_every_roster() {
        // The root falls outside the discovery window, or its file has not been sighted
        // yet: the chain ends at an id nobody has, so the Sub-agent waits — attached to
        // nothing rather than to a guess. Its own nested child waits with it.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        write_codex(
            dir.path(),
            "other",
            &rollout_lines("other", None, "/a/foo", 100),
        );
        let orphan = write_codex(
            dir.path(),
            "sub-1",
            &rollout_lines("sub-1", Some(("root-missing", 1)), "/a/foo", 900),
        );
        write_codex(
            dir.path(),
            "sub-2",
            &rollout_lines("sub-2", Some(("sub-1", 2)), "/a/foo", 400),
        );

        let mut store = codex_store(dir.path());
        let cards = store.scan(now);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "other");
        assert!(cards[0].sub_agent_roster.is_empty());
        assert_eq!(cards[0].tokens_in, 100, "no orphan spend leaks onto a card");
        assert!(store.ingest(&orphan, now).is_none(), "nothing to emit yet");
    }

    #[test]
    fn removing_a_sub_agent_takes_the_chain_below_it_off_the_card_too() {
        // Removing a link breaks every chain that ran through it, and the one rebuild
        // the removal emits has to reflect that — otherwise the grandchild's row, and
        // its spend, sit on the card until something unrelated rebuilds it.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        write_codex(
            dir.path(),
            "root-1",
            &rollout_lines("root-1", None, "/a/foo", 100),
        );
        let middle = write_codex(
            dir.path(),
            "sub-1",
            &rollout_lines("sub-1", Some(("root-1", 1)), "/a/foo", 200),
        );
        write_codex(
            dir.path(),
            "sub-2",
            &rollout_lines("sub-2", Some(("sub-1", 2)), "/a/foo", 400),
        );

        let mut store = codex_store(dir.path());
        assert_eq!(store.scan(now)[0].sub_agent_roster.len(), 2);

        fs::remove_file(&middle).unwrap();
        let Some(Event::Upsert(card)) = store.remove(&middle, now) else {
            panic!("removing a Sub-agent re-emits its root");
        };
        assert_eq!(card.id, "root-1");
        assert!(
            card.sub_agent_roster.is_empty(),
            "the grandchild goes with it: {:?}",
            card.sub_agent_roster
        );
        assert_eq!(card.tokens_in, 100, "and so does its spend");
    }

    #[test]
    fn a_spawn_chain_that_loops_ends_the_walk_rather_than_the_process() {
        // No honest source writes a cycle; a file can claim one. The walk is bounded,
        // so the claim costs the rows and nothing else.
        let dir = tempfile::tempdir().unwrap();
        let now = ts("2026-07-19T10:00:30Z");
        write_codex(
            dir.path(),
            "root-1",
            &rollout_lines("root-1", None, "/a/foo", 100),
        );
        write_codex(
            dir.path(),
            "sub-a",
            &rollout_lines("sub-a", Some(("sub-b", 1)), "/a/foo", 900),
        );
        write_codex(
            dir.path(),
            "sub-b",
            &rollout_lines("sub-b", Some(("sub-a", 1)), "/a/foo", 900),
        );

        let cards = codex_store(dir.path()).scan(now);
        assert_eq!(cards.len(), 1);
        assert!(cards[0].sub_agent_roster.is_empty());
    }

    #[test]
    fn a_running_codex_sub_agent_keeps_its_quiet_parent_working() {
        // The Staleness refinement, on the Codex side: the parent's own rollout has
        // been quiet for 25 minutes, but a Sub-agent that never reached its terminal
        // event is still running — and fanning out is never a human need.
        let dir = tempfile::tempdir().unwrap();
        write_codex(
            dir.path(),
            "root-1",
            &rollout_lines("root-1", None, "/a/foo", 100),
        );
        write_codex(
            dir.path(),
            "sub-1",
            &rollout_lines("sub-1", Some(("root-1", 1)), "/a/foo", 900),
        );
        let mut store = codex_store(dir.path());
        let ingested = ts("2026-07-19T10:00:00Z");
        store.scan(ingested);
        for entry in store.files.values_mut() {
            entry.mtime = ingested;
        }

        let later = ts("2026-07-19T10:25:00Z");
        let card = store
            .snapshot(later)
            .into_iter()
            .find(|s| s.id == "root-1")
            .unwrap();
        assert_eq!(card.status, Status::Active);
        assert!(card.attention.is_none());
        assert_eq!(
            card.sub_agent_roster[0].state,
            crate::model::SubAgentState::Running
        );
    }

    /// Re-ground the Codex Sub-agent join against this machine's real rollouts.
    ///
    /// Every fixture above is hand-written against a format we do not control, which
    /// is exactly how the Sub-agent badge rotted for months without a red test (ADR
    /// 0014). This one reads the corpus instead: it ignores the discovery window (it
    /// ingests each file directly) so history counts, and reports what the join
    /// resolved. **Host-dependent**, so ignored by default, like the liveness probe:
    ///
    /// `cargo test -p sessions codex_corpus -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn the_codex_corpus_resolves_every_sub_agent_to_a_root() {
        let root = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".codex"))
            .join("sessions");
        if !root.is_dir() {
            eprintln!("no Codex corpus at {root:?}; nothing to re-ground against");
            return;
        }
        let now = Utc::now();
        let mut store = codex_store(&root);
        let mut found = Vec::new();
        walk(&root, MAX_SCAN_DEPTH, &mut found);
        found.retain(|p| store.owns_path(p));
        for path in &found {
            store.feed(path, now);
        }

        let tree = store.spawn_tree();
        let (mut subs, mut rooted, mut tokens) = (0u64, 0u64, 0u64);
        for entry in store.files.values() {
            let Some(Folded::SubAgent(sub)) = entry.state.folded() else {
                continue;
            };
            subs += 1;
            tokens += sub.tokens_in;
            rooted += u64::from(tree.root_of(sub.attachment.as_ref()).is_some());
        }
        eprintln!("{rooted} of {subs} Codex Sub-agents rooted, {tokens} input tokens");
        assert!(subs > 0, "the corpus holds no subagent rollouts to check");
        assert_eq!(rooted, subs, "every observed chain resolves to a root");
    }

    /// Re-ground the Claude Sub-agent lifecycle against this machine's real transcripts
    /// — the companion to the Codex test above, and the test that was missing when the
    /// bug it now guards shipped (issue #85).
    ///
    /// The check is deliberately asymmetric. One side is the fold; the other is a raw
    /// text scan for `<task-notification>` blocks that knows nothing about record types,
    /// turns, or queues. Every spawn the dumb side finds an ending for, the fold must
    /// also report ended. Reading only `user` turns passed every hand-written fixture in
    /// this repo and failed this on 33 of 92 spawns.
    ///
    /// **Host-dependent**, so ignored by default, like the Codex one:
    ///
    /// `cargo test -p sessions claude_corpus -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn the_claude_corpus_reads_every_ending_it_states() {
        let root = match std::env::var_os("RIKU_ROOT") {
            Some(r) => PathBuf::from(r),
            None => match crate::default_root() {
                Some(r) => r,
                None => return,
            },
        };
        if !root.is_dir() {
            eprintln!("no Claude corpus at {root:?}; nothing to re-ground against");
            return;
        }
        let now = Utc::now();
        let mut store = SessionStore::new(vec![Box::new(ClaudeSource::new(root.clone()))]);
        let mut found = Vec::new();
        walk(&root, MAX_SCAN_DEPTH, &mut found);
        found.retain(|p| store.owns_path(p));
        for path in &found {
            // Directly, so the discovery window does not hide history.
            store.feed(path, now);
        }

        // The independent side, per transcript: the endings *that file's own bytes*
        // state, by spawning tool-use id. No JSON, no record types — just the tags.
        // Kept per file rather than corpus-wide so one transcript quoting another's
        // notification (which a transcript of a session reading its own logs does) can
        // never vouch for a row it does not own.
        let endings = |path: &Path| -> HashMap<String, String> {
            let mut stated = HashMap::new();
            let Ok(text) = fs::read_to_string(path) else {
                return stated;
            };
            for block in text.split("<task-notification>").skip(1) {
                let block = block.split("</task-notification>").next().unwrap_or(block);
                let between = |open: &str, close: &str| {
                    let s = block.find(open)? + open.len();
                    let e = block[s..].find(close)?;
                    Some(block[s..s + e].trim().to_string())
                };
                if let (Some(tuid), Some(status)) = (
                    between("<tool-use-id>", "</tool-use-id>"),
                    between("<status>", "</status>"),
                ) {
                    // Latest wins, and a file is read in its own order.
                    stated.insert(tuid, status);
                }
            }
            stated
        };

        let (mut spawns, mut ended, mut unread) = (0u64, 0u64, Vec::new());
        for (path, entry) in &store.files {
            let Some(Folded::AgentSession(p)) = entry.state.folded() else {
                continue;
            };
            if p.sub_agent_roster.is_empty() {
                continue;
            }
            let stated = endings(path);
            for row in &p.sub_agent_roster {
                spawns += 1;
                let Some(word) = stated.get(&row.spawn_key) else {
                    continue; // no ending stated at all: ADR 0014's parent-dominance case
                };
                ended += 1;
                if row.outcome.as_deref() != Some(word.as_str()) {
                    unread.push(format!(
                        "{} states {word:?}, the fold reads {:?}",
                        row.spawn_key, row.outcome
                    ));
                }
            }
        }
        eprintln!(
            "{spawns} Claude spawns, {ended} with an ending stated, {} unread",
            unread.len()
        );
        for u in unread.iter().take(10) {
            eprintln!("  {u}");
        }
        assert!(spawns > 0, "the corpus holds no Claude spawns to check");
        assert!(
            unread.is_empty(),
            "{} spawn(s) ended in the corpus without the fold reading it",
            unread.len()
        );
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
