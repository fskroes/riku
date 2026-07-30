//! Three things live here: the two Claude Code [`Fold`]s — [`Accumulator`] for an
//! Agent Session transcript and [`ClaudeSubAgentFold`] for a Sub-agent's own — and
//! the shared incremental tail ([`FileState`]) that drives any source's fold.
//!
//! [`Accumulator`] decodes Claude Code transcript entries and folds them into a
//! [`Projection`]; [`ClaudeSubAgentFold`] folds a child file into a
//! [`SubAgentProjection`], which is never a card. Which of the two a file gets is
//! the Session Source's decision, taken from the path (see `source.rs`).
//! [`FileState`] is source-agnostic: it owns the byte-offset bookkeeping,
//! truncation reset, malformed-line handling, and exposing what its [`Fold`] has
//! produced ([`FileState::folded`]). It does *not* own the final card: a Sub-agent's
//! spend lives in a different file, so the store joins the roster on and calls
//! [`assemble`] (see `store::build`). [`FileState::build`] assembles without that
//! join — the shape a session that never fanned out has, and the seam a fold's own
//! tests build against.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::warn;

use crate::attention::{AttentionReducer, NeedEvidence, Observation};
use crate::fold::{
    assemble, first_line, project_from_cwd, Attachment, Fold, Folded, Projection,
    SubAgentProjection,
};
use crate::liveness::ProcessLiveness;
use crate::model::{AttentionCause, Session, SubAgent, SubAgentState, Tool};
use crate::parse::{parse_entry, Entry, ToolUseInfo};

/// The Claude Code tool that spawns a Sub-agent. Its tool-use `id` is the key the
/// Sub-agent's own metadata sidecar joins back on, and its input `description` is
/// the Errand.
///
/// It was once called `Task`. Zero transcripts in the corpus contain a `Task`
/// tool-use; 30 parent transcripts contain `Agent`. That rename is the first of the
/// four independent breakages that kept the badge from ever rendering (ADR 0014).
const AGENT_TOOL: &str = "Agent";

/// The Claude Code tool that sends a message to a Sub-agent that has already
/// finished, putting it back to work. Its `to` field names the Sub-agent by the
/// `task-id` its completion notification stated — so it is the one record that says a
/// finished Sub-agent is Running again.
///
/// Read from the same corpus as everything else here, where exactly one resume was
/// observed. Like [`AGENT_TOOL`] it is a tool name, and tool names have been renamed
/// under us before (issue #78): a rename costs a resumed Sub-agent its return to
/// Running, and nothing else — the row keeps the last outcome the source stated.
const RESUME_TOOL: &str = "SendMessage";

/// One Sub-agent as its parent's own transcript knows it: the spawn that started it,
/// what it was sent to do, and the latest word on how it ended.
#[derive(Debug)]
struct SpawnedSubAgent {
    /// The spawning `Agent` tool-use id — this row's join key, both to the
    /// Sub-agent's own file and to the notification that ends its first run.
    key: String,
    /// The Errand, from the spawn's `description`.
    errand: Option<String>,
    /// The source's own id for this Sub-agent, learned from its first notification.
    /// It is what a resuming message addresses, and what every later notification
    /// joins on once the spawn's own id has stopped being the one stated.
    task_id: Option<String>,
    /// The latest verbatim outcome word, or `None` while it is running. Latest rather
    /// than final: a Sub-agent can be resumed after it ends.
    outcome: Option<String>,
}

/// The Claude Code fold: a running projection of a single transcript. Fold entries
/// in file order via [`Accumulator::apply`]; token counts and "latest" fields
/// update in place. Attention lifecycle is delegated entirely to the shared
/// [`AttentionReducer`] — this fold only *translates* each entry into normalized
/// need/resolution [`Observation`]s.
#[derive(Debug, Default)]
pub struct Accumulator {
    id: Option<String>,
    project: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    latest_timestamp: Option<DateTime<Utc>>,
    /// Model of the latest *assistant* entry (not just the latest entry).
    model: Option<String>,
    activity: Option<String>,
    tokens_in: u64,
    tokens_out: u64,
    /// Every Sub-agent this session has spawned, in spawn order. Entries are never
    /// retired — the roster carries running and finished alike — and what retires the
    /// *state* of one is its completion notification, never a spawn's `tool_result`,
    /// which is a launch acknowledgement (see [`Accumulator::apply`]).
    spawns: Vec<SpawnedSubAgent>,
    attention: AttentionReducer,
}

impl Accumulator {
    /// Fold one already-parsed relevant entry into the projection.
    pub fn apply(&mut self, entry: Entry) {
        if let Some(ts) = entry.timestamp {
            // Entries arrive in file (chronological) order, but guard anyway.
            if self.latest_timestamp.is_none_or(|cur| ts >= cur) {
                self.latest_timestamp = Some(ts);
            }
        }

        // Identity first, and unconditionally — whatever else this entry is, it says
        // which session's file this is. ADR 0014's second breakage was an early
        // return positioned *above* this assignment, which left `projection()`
        // returning `None` and discarded every fold that took the branch. Nothing
        // below may return before this point.
        if let Some(id) = entry.session_id {
            self.id = Some(id);
        }
        if let Some(cwd) = entry.cwd {
            self.project = Some(project_from_cwd(&cwd));
            self.cwd = Some(cwd);
        }
        if entry.git_branch.is_some() {
            self.branch = entry.git_branch;
        }

        // Sub-agent turns left the parent transcript: they live in the Sub-agent's
        // own file, folded by `ClaudeSubAgentFold`, and parent-side sidechain entries
        // number zero across the whole corpus. What ADR 0014 retired is the *folding*
        // that used to happen here — the parallel sub-token counters whose output
        // reached the bin, and that early return's position.
        //
        // Ignoring such an entry is not that. A legacy transcript still inside the
        // discovery window would otherwise have its Sub-agent's turns folded as the
        // parent's *own*: priced at the parent's model, and — the part that matters —
        // its activity line and its Attention lifecycle reaching the parent's card,
        // inferring a human need from an agent-level event, which ADR 0010 rules out.
        // This entry belongs to a Sub-agent, and the Sub-agent's own file counts it.
        if entry.is_sidechain {
            return;
        }

        self.tokens_in += entry.input_tokens;
        self.tokens_out += entry.output_tokens;

        // A completion notification: the one record that says a Sub-agent has actually
        // finished, and how. It is addressed to the orchestrator, which reads it and
        // carries on — in 23 of 24 observed non-`completed` notifications the parent's
        // next entry came a median of 0.7 seconds later — so it moves a roster entry
        // and nothing else. It raises no need of its own (ADR 0010 rules out inferring
        // a human need from an agent-level event), and it answers none either: a
        // person still waiting to approve something is still waiting after a Sub-agent
        // dies. That second half is what `notified` carries down to the branches below.
        let notifications = entry.task_notifications;
        let notified = !notifications.is_empty();
        for note in notifications {
            self.note_completion(note);
        }

        // Attention Since prefers the entry's own timestamp, falling back to the
        // latest one seen so far (set just above) when a record carries none.
        let at = entry.timestamp.or(self.latest_timestamp);

        if entry.is_api_error {
            // A synthetic API-error record (model `<synthetic>`, no real content):
            // raise a Session error but never overwrite the card's model/activity.
            self.attention.apply(Observation::Need {
                key: "error".into(),
                cause: AttentionCause::Error,
                evidence: NeedEvidence::Error {
                    text: entry.activity,
                },
                at,
            });
        } else if entry.is_assistant {
            if entry.model.is_some() {
                self.model = entry.model;
            }
            if entry.activity.is_some() {
                self.activity = entry.activity;
            }
            // An `Agent` tool-use spawns a Sub-agent: record the spawn and its Errand.
            // It is *not* a human-input wait (the Sub-agent runs on its own), so it is
            // kept out of the awaiting decision below — otherwise a fanning-out turn
            // would masquerade as needing attention, the exact false pull we remove.
            let mut human_waits: Vec<ToolUseInfo> = Vec::new();
            for tool in entry.tool_uses {
                match tool.name.as_deref() {
                    Some(AGENT_TOOL) => {
                        if let Some(id) = tool.id {
                            if !self.spawns.iter().any(|s| s.key == id) {
                                self.spawns.push(SpawnedSubAgent {
                                    key: id,
                                    errand: tool.detail,
                                    task_id: None,
                                    outcome: None,
                                });
                            }
                        }
                    }
                    // Sending a finished Sub-agent back to work. Unlike a spawn this
                    // is still a tool call like any other — the turn ends on it and
                    // its own `tool_result` answers it — so it stays in the awaiting
                    // decision below; only the roster row changes here.
                    Some(RESUME_TOOL) => {
                        if let Some(to) = tool.recipient.as_deref() {
                            self.note_resume(to);
                        }
                        human_waits.push(tool);
                    }
                    _ => human_waits.push(tool),
                }
            }
            // Waiting-on-human = the turn ended to call a (non-`Agent`) tool. Prefer the
            // explicit `stop_reason`; fall back to the presence of a `tool_use` block
            // only when `stop_reason` is absent. This stops a mid-run tool_use or a
            // cleanly-ended (`end_turn`) turn from masquerading as a wait.
            let awaiting = match entry.stop_reason.as_deref() {
                Some("tool_use") => !human_waits.is_empty(),
                Some(_) => false,
                None => !human_waits.is_empty(),
            };
            if awaiting {
                // The trailing tool_use is the one the turn is blocked on. Claude
                // transcripts do not mark *why* a call sits unanswered (approval vs
                // mid-run), so the cause is the honest generic fallback rather than
                // an inferred Approval (ADR 0010: keep the card honest and generic).
                let tool = human_waits.into_iter().next_back().unwrap_or_default();
                self.attention.apply(Observation::Need {
                    key: tool.id.unwrap_or_else(|| "tool".into()),
                    cause: AttentionCause::Input,
                    evidence: NeedEvidence::Tool {
                        name: tool.name.unwrap_or_default(),
                        detail: tool.detail,
                    },
                    at,
                });
            } else {
                // A cleanly-ended turn is forward progress: it supersedes any
                // pending error/approval (recovery needs no extra bookkeeping).
                self.attention.apply(Observation::Superseded);
            }
        } else if entry.tool_result_ids.is_empty() {
            // A plain user turn resuming after an error/approval is forward progress —
            // unless the turn *is* the notification, which the person never typed and
            // which says nothing about what they were asked for.
            if !notified {
                self.attention.apply(Observation::Superseded);
            }
        } else {
            // A tool_result answers its tool_use by id — a correlated resolution.
            //
            // It does *not* end a Sub-agent. An `Agent` spawn's tool_result is a launch
            // acknowledgement — "Async agent launched successfully… you will be
            // notified automatically when it completes" — arriving ~2s after the spawn
            // against children that run up to 20 minutes. Retiring the roster entry
            // here (as the old rule did) would empty the badge two seconds after every
            // spawn. Completion arrives separately, up to 20 minutes later, as the
            // notification [`Accumulator::note_completion`] reads.
            for id in entry.tool_result_ids {
                self.attention.apply(Observation::Resolved { key: id });
            }
        }
    }

    /// Record how a Sub-agent ended, joining the notification back to the spawn that
    /// started it.
    ///
    /// **By id, never by count.** 101 task-ids appear against 59 spawns — backgrounded
    /// commands and monitor events notify under the same tag — so a notification
    /// naming no spawn of ours moves nothing and creates nothing. And **latest wins**:
    /// a Sub-agent can be resumed after it ends, so the roster carries the newest word
    /// rather than treating the first as final.
    fn note_completion(&mut self, note: crate::parse::TaskNotification) {
        let Some(spawn) = self.spawns.iter_mut().find(|s| {
            s.key == note.tool_use_id
                // A run that is not the first notifies under the id of the message
                // that started it, not the spawn's. The `task-id` is the identity that
                // outlives any one tool-use id — every notification in the corpus
                // states one — so it is what a later ending joins on.
                || (note.task_id.is_some() && s.task_id == note.task_id)
        }) else {
            return;
        };
        if spawn.task_id.is_none() {
            spawn.task_id = note.task_id;
        }
        spawn.outcome = Some(note.status);
    }

    /// A message addressed to a Sub-agent that had finished: it is working again, so
    /// the row drops the word it ended on and returns to Running.
    ///
    /// The recipient is matched against the id the *source* stated in a notification,
    /// so a message to something that never notified — or that is not a Sub-agent at
    /// all — matches nothing.
    fn note_resume(&mut self, to: &str) {
        if let Some(spawn) = self
            .spawns
            .iter_mut()
            .find(|s| s.task_id.as_deref() == Some(to))
        {
            spawn.outcome = None;
        }
    }
}

impl Fold for Accumulator {
    /// Parse and fold one raw line. Blank lines are ignored. A parse failure is
    /// reported to the caller (`false`) so it can distinguish a committed but
    /// malformed line from a mid-write fragment.
    fn apply_line(&mut self, line: &str) -> bool {
        if line.trim().is_empty() {
            return true;
        }
        match parse_entry(line) {
            Ok(Some(entry)) => {
                self.apply(entry);
                true
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    fn reset(&mut self) {
        *self = Accumulator::default();
    }

    fn projection(&self) -> Option<Folded> {
        let id = self.id.clone()?;
        Some(Folded::AgentSession(Projection {
            id,
            tool: Tool::Claude,
            project: self.project.clone().unwrap_or_default(),
            model: self.model.clone(),
            branch: self.branch.clone(),
            cwd: self.cwd.clone(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            // This side of the roster: what the parent recorded about each Sub-agent
            // it sent out. It exists from the instant of the spawn, before any child
            // file does, and says what the Sub-agent was sent to do. What each one
            // *spent* comes from its own file, joined onto these rows by the spawn's
            // tool-use id in the store. Neither side alone is required for a row.
            //
            // Until that join runs, a row's identity is the spawn key — the only id
            // the parent's own transcript names. The child file supplies the
            // Sub-agent's own `agentId`, and its depth, when it is discovered.
            sub_agent_roster: self
                .spawns
                .iter()
                .map(|spawn| SubAgent {
                    id: spawn.key.clone(),
                    spawn_key: spawn.key.clone(),
                    errand: spawn.errand.clone(),
                    // Running until a notification says otherwise — the acknowledgement
                    // that answers the spawn ~2s later says only that it launched.
                    state: match spawn.outcome {
                        Some(_) => SubAgentState::Finished,
                        None => SubAgentState::Running,
                    },
                    outcome: spawn.outcome.clone(),
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: None,
                    model: None,
                    // A spawn recorded in an Agent Session's own transcript is by
                    // definition one level down. Emitting 0 here and 1 from the
                    // sidecar would flip the same row's depth the moment its child
                    // file was discovered.
                    depth: 1,
                    last_event_at: None,
                })
                .collect(),
            activity: self.activity.clone(),
            last_event_at: self.latest_timestamp,
            attention: self.attention.current(),
        }))
    }
}

/// What a Sub-agent's metadata sidecar states about it: the Errand it was sent on,
/// the tool-use id of the spawn that created it, and how deep it was spawned.
///
/// Claude writes the sidecar beside the child transcript, at **spawn** rather than
/// completion — verified across 58 of 58 children that ran over 30 seconds. So the
/// Errand and the link back to the parent both exist the instant a Sub-agent does,
/// and no roster field is missing while one runs.
///
/// Every field is optional: a sidecar that is absent, unreadable, or drifted in
/// shape costs the roster row its label, never the row.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubAgentMeta {
    /// The Errand, verbatim — the same string the spawning `Agent` tool-use carries.
    pub description: Option<String>,
    /// The spawning tool-use id: the key this Sub-agent's row joins its parent's
    /// spawn record on.
    pub tool_use_id: Option<String>,
    /// 1 for a Sub-agent of an Agent Session, 2 for one a Sub-agent spawned itself.
    ///
    /// Tolerant of a drifted type, unlike the rest of the struct: serde is
    /// all-or-nothing per struct, so a `spawnDepth` that arrives as a string would
    /// otherwise discard the Errand and the join key with it — losing the row's
    /// label *and* its link to the parent's spawn over the least important field.
    #[serde(default, deserialize_with = "lenient_depth")]
    pub spawn_depth: u32,
}

/// A `spawnDepth` of any other JSON shape reads as 0 rather than failing the parse.
fn lenient_depth<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(d)
        .ok()
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or_default())
}

/// A sidecar larger than this is not one Claude Code wrote — real ones are ~120
/// bytes. Read no further rather than pulling an arbitrary file into memory.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

impl SubAgentMeta {
    /// Read the sidecar at `path`, or an empty one when it is missing, unreadable,
    /// oversized, or not the shape we expect. Never an error: a Sub-agent whose
    /// sidecar cannot be read still folds, and its row still shows what it spent.
    ///
    /// The Errand is bounded here, at the one point it enters the projection, so it
    /// is bounded *before* retention and transport. Every other string the card
    /// carries already is — the activity line and the parent's own copy of this same
    /// Errand both run through [`first_line`], and Attention Evidence is bounded by
    /// design (ADR 0010). Without this the two sides of the roster's union would
    /// disagree: the same Errand capped at 80 chars when the parent recorded it,
    /// unbounded when the sidecar supplied it.
    pub fn read(path: &std::path::Path) -> Self {
        let mut meta: SubAgentMeta = read_capped(path, MAX_SIDECAR_BYTES)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        meta.description = meta.description.as_deref().and_then(first_line);
        meta
    }
}

/// Read at most `cap` bytes of `path` as UTF-8, or `None` if it cannot be read or
/// runs past the cap.
fn read_capped(path: &std::path::Path, cap: u64) -> Option<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap + 1)
        .read_to_string(&mut buf)
        .ok()?;
    (buf.len() as u64 <= cap).then_some(buf)
}

/// The Claude Code Sub-agent fold: one child transcript at
/// `<project>/<root-uuid>/subagents/agent-<agentId>.jsonl`.
///
/// It folds the child in full — every assistant turn's usage, priced at the child's
/// own (possibly cheaper) model — and produces a [`SubAgentProjection`], which is
/// not a card. Both ids come from the path and the Errand from the sidecar, so a
/// Sub-agent has an identity and a purpose from the moment its file is discovered.
///
/// **The path is authoritative for the root.** A child entry's `sessionId` carries
/// the root's id too — Claude stamps it at any spawn depth, and it agreed with the
/// containing directory across all 70 child transcripts in the corpus — so reading
/// it would decide nothing that the directory has not already decided. It is
/// therefore only *checked*, never preferred: file content is not allowed to steer a
/// roster row onto another session's card, which is the one input to the cross-file
/// join an attacker-influenced file could otherwise control.
#[derive(Debug)]
pub struct ClaudeSubAgentFold {
    id: String,
    /// The root session id, from the containing directory's name.
    root_from_path: String,
    /// What the sidecar beside this transcript said at spawn time.
    meta: SubAgentMeta,
    /// Where that sidecar lives, so it can be re-read if it had not landed yet.
    meta_path: PathBuf,
    /// Model of the latest assistant entry in this child's own transcript.
    model: Option<String>,
    tokens_in: u64,
    tokens_out: u64,
    /// Running cost, priced per assistant entry's own model as it is folded in.
    cost_usd: f64,
    latest_timestamp: Option<DateTime<Utc>>,
}

impl ClaudeSubAgentFold {
    /// A fold for the Sub-agent `id`, rooted at `root_session_id` (both read from
    /// the path by the Session Source), reading its sidecar at `meta_path`.
    pub fn new(id: String, root_session_id: String, meta_path: PathBuf) -> Self {
        ClaudeSubAgentFold {
            id,
            root_from_path: root_session_id,
            meta: SubAgentMeta::read(&meta_path),
            meta_path,
            model: None,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            latest_timestamp: None,
        }
    }

    /// Re-read the sidecar while it has still told us nothing.
    ///
    /// Claude writes it at spawn, before the Sub-agent's first turn — verified across
    /// 58 of 58 children that ran over 30 seconds — but nothing *enforces* that the
    /// watcher sights the two files in that order. Sighting the transcript first
    /// would otherwise leave this Sub-agent unlabelled and unjoined for the life of
    /// the process, and an unjoined child shows up as a *second* row beside its own
    /// spawn record: one Sub-agent, counted twice on the badge. Retrying until the
    /// sidecar answers makes the order not matter.
    fn refresh_meta(&mut self) {
        if self.meta == SubAgentMeta::default() {
            self.meta = SubAgentMeta::read(&self.meta_path);
        }
    }
}

impl Fold for ClaudeSubAgentFold {
    fn apply_line(&mut self, line: &str) -> bool {
        if line.trim().is_empty() {
            return true;
        }
        let entry = match parse_entry(line) {
            Ok(Some(entry)) => entry,
            Ok(None) => return true,
            Err(_) => return false,
        };
        // A turn arriving is the cue to look for a sidecar we have not seen yet: it
        // means this Sub-agent is running, so its sidecar was written. This is the
        // one file read reachable from line handling, and it is bounded — it stops
        // the first time the sidecar answers, and never runs at all in the ordinary
        // case where the sidecar was already there when the fold was built.
        self.refresh_meta();
        if let Some(ts) = entry.timestamp {
            if self.latest_timestamp.is_none_or(|cur| ts >= cur) {
                self.latest_timestamp = Some(ts);
            }
        }
        // Every entry in this file is the Sub-agent's own traffic (Claude marks it
        // `isSidechain`), so the flag decides nothing here — the path already did.
        // The entry's `sessionId` is the root's too, but the path already stated it
        // and is authoritative; a disagreement is a file that is not what its
        // location says it is, and it is ignored rather than followed.
        if let Some(id) = entry.session_id {
            if id != self.root_from_path {
                warn!(
                    sub_agent = %self.id,
                    stated = %id,
                    path_says = %self.root_from_path,
                    "ignoring a Sub-agent entry naming a different root than its directory"
                );
            }
        }
        if entry.is_assistant {
            self.tokens_in += entry.input_tokens;
            self.tokens_out += entry.output_tokens;
            if let Some(cost) = crate::pricing::estimate_cost_usd(
                entry.model.as_deref(),
                entry.input_tokens,
                entry.output_tokens,
            ) {
                self.cost_usd += cost;
            }
            if entry.model.is_some() {
                self.model = entry.model;
            }
        }
        true
    }

    /// Throw away what was folded, but not the identity the path stated — a rewrite
    /// re-reads the same Sub-agent's file, and its sidecar is re-read with it.
    fn reset(&mut self) {
        *self = ClaudeSubAgentFold::new(
            self.id.clone(),
            self.root_from_path.clone(),
            self.meta_path.clone(),
        );
    }

    fn projection(&self) -> Option<Folded> {
        Some(Folded::SubAgent(SubAgentProjection {
            id: self.id.clone(),
            // Claude names the root outright, at any spawn depth, so there is no chain
            // to walk — the cross-file join it charges for instead is the spawn key.
            attachment: Some(Attachment::Root(self.root_from_path.clone())),
            spawn_key: self.meta.tool_use_id.clone(),
            errand: self.meta.description.clone(),
            depth: self.meta.spawn_depth,
            // A Claude Sub-agent's own file never says that it stopped, let alone how;
            // the parent's notification does, and the two sides meet in `merge_roster`.
            state: SubAgentState::Running,
            outcome: None,
            tool: Tool::Claude,
            model: self.model.clone(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            cost_usd: self.cost_usd,
            last_event_at: self.latest_timestamp,
        }))
    }
}

/// Incremental tail state for one transcript file: the source's [`Fold`] plus the
/// byte offset consumed so far. Source-agnostic — the fold decides how to decode
/// each line.
pub struct FileState {
    fold: Box<dyn Fold>,
    offset: u64,
}

impl FileState {
    /// Wrap a fresh fold for one file.
    pub fn new(fold: Box<dyn Fold>) -> Self {
        FileState { fold, offset: 0 }
    }

    /// Feed the bytes read from the current `offset` to the end of the file.
    ///
    /// Only `\n`-terminated lines are consumed; a trailing fragment without a
    /// newline is a mid-write line — left unconsumed and retried on the next
    /// change. A `\n`-terminated line that fails to parse is a genuinely
    /// malformed record: skipped with a `warn`, and the offset still advances so
    /// the tail never gets stuck.
    pub fn feed(&mut self, buf: &[u8]) {
        let mut pos = 0;
        while let Some(rel) = memchr(b'\n', &buf[pos..]) {
            let line_bytes = &buf[pos..pos + rel];
            let ok = match std::str::from_utf8(line_bytes) {
                Ok(s) => self.fold.apply_line(s),
                Err(_) => false,
            };
            if !ok {
                warn!(
                    offset = self.offset + pos as u64,
                    "skipping malformed transcript line"
                );
            }
            pos += rel + 1;
        }
        self.offset += pos as u64;
    }

    /// Byte offset consumed so far. The watcher reads from here on the next change.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// The file shrank below our offset (truncated or rewritten) — throw away
    /// accumulated state and re-parse from the start.
    pub fn reset(&mut self) {
        self.fold.reset();
        self.offset = 0;
    }

    /// Build the UI [`Session`] without process-liveness data — the mtime rule
    /// alone drives status. Equivalent to
    /// [`build_with_liveness`](Self::build_with_liveness) with
    /// [`ProcessLiveness::Unknown`].
    pub fn build(&self, mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<Session> {
        self.build_with_liveness(mtime, now, ProcessLiveness::Unknown)
    }

    /// Build the UI [`Session`], given the file's mtime and the session's process
    /// verdict (together they drive status — see [`status_for`]). Returns `None`
    /// if the fold has no projection yet, or produced a Sub-agent — which is folded
    /// in full but is not a card.
    ///
    /// This is the **no-roster** build: it sees only what this one file's fold
    /// produced, so a card built here carries the spawns its own transcript recorded
    /// and nothing of what its Sub-agents spent — that lives in other files. The
    /// store joins those on before assembling; this is the seam a fold's own tests
    /// build against.
    pub fn build_with_liveness(
        &self,
        mtime: DateTime<Utc>,
        now: DateTime<Utc>,
        liveness: ProcessLiveness,
    ) -> Option<Session> {
        // The projection is this fold's job; turning it into a card is the shared,
        // pure `assemble` seam (see `fold::assemble`), which every source crosses.
        match self.fold.projection()? {
            Folded::AgentSession(p) => Some(assemble(p, mtime, now, liveness)),
            Folded::SubAgent(_) => None,
        }
    }

    /// What this file's fold has produced — an Agent Session projection or a
    /// Sub-agent one. The store reads this to index Sub-agents by their root before
    /// building any card.
    pub fn folded(&self) -> Option<Folded> {
        self.fold.projection()
    }
}

/// Minimal byte search; avoids pulling in the `memchr` crate for one call site.
fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::model::{AttentionCause, Status};
    use crate::source::SessionSource;

    /// The cause of a session's current Attention, or `None`.
    fn cause(s: &Session) -> Option<AttentionCause> {
        s.attention.as_ref().map(|a| a.cause)
    }

    fn claude() -> FileState {
        FileState::new(Box::new(Accumulator::default()))
    }

    /// The root Agent Session id Claude stamps on every Sub-agent entry, at any
    /// spawn depth — and names the directory holding that session's `subagents/`.
    const ROOT: &str = "11111111-2222-3333-4444-555555555555";

    /// A Claude Sub-agent's own transcript, at the path Claude Code writes it to:
    /// `<project>/<root-uuid>/subagents/agent-<agentId>.jsonl`. The directory is
    /// flat — a depth-2 child sits beside its depth-1 spawner.
    fn sub_agent_path(root: &Path, agent_id: &str) -> PathBuf {
        root.join("-Users-x-repos-foo")
            .join(ROOT)
            .join("subagents")
            .join(format!("agent-{agent_id}.jsonl"))
    }

    /// Write a Sub-agent's transcript and the metadata sidecar Claude writes beside
    /// it **at spawn**, returning the transcript's path.
    ///
    /// The sidecar's four fields are the ones present on 70 of 70 real sidecars:
    /// what the Sub-agent was sent to do, the agent type, the tool-use id of the
    /// spawn it joins back to, and how deep it was spawned.
    fn write_sub_agent(
        root: &Path,
        agent_id: &str,
        meta: serde_json::Value,
        lines: &[String],
    ) -> PathBuf {
        let path = sub_agent_path(root, agent_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path.with_file_name(format!("agent-{agent_id}.meta.json")),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The four-field sidecar, as Claude writes it.
    fn meta(errand: &str, tool_use_id: &str, depth: u32) -> serde_json::Value {
        serde_json::json!({
            "agentType": "Explore",
            "description": errand,
            "toolUseId": tool_use_id,
            "spawnDepth": depth,
        })
    }

    /// An Agent Session transcript, flat under its project directory.
    const SESSION_PATH: &str = "/r/-Users-x-repos-foo/deadbeef.jsonl";

    /// The fold the Claude Session Source hands back for `path` — the seam where a
    /// source decides which of the two things it is about to read, and reads the
    /// sidecar beside a Sub-agent's transcript.
    fn claude_fold_at(root: &Path, path: &Path) -> Box<dyn Fold> {
        crate::source::ClaudeSource::new(root.to_path_buf()).new_fold(path)
    }

    fn claude_fold(path: &str) -> Box<dyn Fold> {
        crate::source::ClaudeSource::new("/r".into()).new_fold(std::path::Path::new(path))
    }

    /// One assistant entry from a Sub-agent's own transcript file, in the shape
    /// Claude writes it: `isSidechain`, its own `agentId`, the **root** session's id
    /// on `sessionId` (at any depth), the parent's `cwd` and `gitBranch` verbatim,
    /// and the child's own model — which may be cheaper than the orchestrator's.
    fn sub_agent_assistant(agent_id: &str, model: &str, tin: u64, tout: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "isSidechain": true,
            "agentId": agent_id,
            "sessionId": ROOT,
            "cwd": "/Users/x/repos/foo",
            "gitBranch": "main",
            "timestamp": "2026-07-19T10:00:10Z",
            "message": {
                "model": model,
                "usage": { "input_tokens": tin, "output_tokens": tout },
                "content": [{ "type": "text", "text": "sub work" }]
            }
        })
        .to_string()
    }

    #[test]
    fn a_sub_agent_transcript_folds_in_full_and_states_a_sub_agent_projection() {
        // Folded in full — identity assigned, Errand and join key read from the
        // sidecar, spend counted and priced at the child's own model — and *stated*
        // as a Sub-agent rather than coming back empty (ADR 0014).
        let dir = tempfile::tempdir().unwrap();
        let path = write_sub_agent(
            dir.path(),
            "a1b2c3",
            meta("map the parser", "toolu_a", 1),
            &[
                sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 200, 20),
                sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 800, 80),
            ],
        );
        let mut fs = FileState::new(claude_fold_at(dir.path(), &path));
        fs.feed(&std::fs::read(&path).unwrap());

        let Some(Folded::SubAgent(sub)) = fs.folded() else {
            panic!("a Sub-agent transcript states a Sub-agent projection");
        };
        assert_eq!(sub.id, "a1b2c3");
        assert_eq!(sub.attachment, Some(Attachment::Root(ROOT.into())));
        assert_eq!(sub.spawn_key.as_deref(), Some("toolu_a"));
        assert_eq!(sub.errand.as_deref(), Some("map the parser"));
        assert_eq!(sub.depth, 1);
        assert_eq!(sub.tool, Tool::Claude);
        assert_eq!(sub.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(sub.tokens_in, 1_000);
        assert_eq!(sub.tokens_out, 100);
        assert_eq!(sub.last_event_at, Some(ts("2026-07-19T10:00:10Z")));
        // Priced at Haiku, the model it actually ran — 1k in + 100 out.
        assert!((sub.cost_usd - (0.001 * 0.80 + 0.0001 * 4.00)).abs() < 1e-12);
    }

    #[test]
    fn a_sub_agent_transcript_yields_no_card() {
        // Not because the fold came back empty, but because a Sub-agent projection
        // is not a card.
        let dir = tempfile::tempdir().unwrap();
        let path = write_sub_agent(
            dir.path(),
            "a1b2c3",
            meta("map the parser", "toolu_a", 1),
            &[sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 200, 20)],
        );
        let mut fs = FileState::new(claude_fold_at(dir.path(), &path));
        fs.feed(&std::fs::read(&path).unwrap());
        assert!(fs
            .build(ts("2026-07-19T10:00:20Z"), ts("2026-07-19T10:00:30Z"))
            .is_none());
    }

    #[test]
    fn a_sub_agent_has_an_errand_and_a_parent_before_a_line_arrives() {
        // The sidecar is written at spawn, not completion — verified across 58 of 58
        // children that ran over 30 seconds — so a Sub-agent has an identity, an
        // Errand, and a link back to its spawn from the moment its file exists. No
        // roster field is missing while one runs.
        let dir = tempfile::tempdir().unwrap();
        let path = write_sub_agent(
            dir.path(),
            "a1b2c3",
            meta("map the parser", "toolu_a", 1),
            &[],
        );
        let Some(Folded::SubAgent(sub)) = claude_fold_at(dir.path(), &path).projection() else {
            panic!("a Sub-agent transcript states a Sub-agent projection");
        };
        assert_eq!(sub.id, "a1b2c3");
        // Before any entry, the root is the containing directory's name.
        assert_eq!(sub.attachment, Some(Attachment::Root(ROOT.into())));
        assert_eq!(sub.errand.as_deref(), Some("map the parser"));
        assert_eq!(sub.spawn_key.as_deref(), Some("toolu_a"));
        assert_eq!(sub.tokens_in, 0);
    }

    #[test]
    fn a_sidecar_that_lands_after_the_transcript_is_still_read() {
        // Claude writes the sidecar at spawn, but nothing enforces which of the two
        // files the watcher sights first. Sighting the transcript first must not cost
        // this Sub-agent its Errand and its join key for the life of the process —
        // which would also split it into two roster rows beside its own spawn record.
        let dir = tempfile::tempdir().unwrap();
        let path = sub_agent_path(dir.path(), "a1b2c3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 40, 4)
            ),
        )
        .unwrap();

        // The fold is built while only the transcript exists.
        let mut fs = FileState::new(claude_fold_at(dir.path(), &path));
        let Some(Folded::SubAgent(sub)) = fs.folded() else {
            panic!("a Sub-agent projection");
        };
        assert_eq!(sub.errand, None, "nothing to read yet");

        // The sidecar lands, and the next turn is the cue to look again.
        std::fs::write(
            path.with_file_name("agent-a1b2c3.meta.json"),
            meta("map the parser", "toolu_a", 1).to_string(),
        )
        .unwrap();
        fs.feed(&std::fs::read(&path).unwrap());

        let Some(Folded::SubAgent(sub)) = fs.folded() else {
            panic!("a Sub-agent projection");
        };
        assert_eq!(sub.errand.as_deref(), Some("map the parser"));
        assert_eq!(sub.spawn_key.as_deref(), Some("toolu_a"));
    }

    #[test]
    fn a_drifted_sidecar_costs_the_label_not_the_join() {
        // Drift must not be all-or-nothing. `spawnDepth` arriving as a string is the
        // least important field on the sidecar; losing the join key with it would
        // split one Sub-agent into two roster rows over it.
        let dir = tempfile::tempdir().unwrap();
        let path = write_sub_agent(
            dir.path(),
            "a1b2c3",
            serde_json::json!({
                "description": "map the parser",
                "toolUseId": "toolu_a",
                "spawnDepth": "1",
            }),
            &[sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 40, 4)],
        );
        let mut fs = FileState::new(claude_fold_at(dir.path(), &path));
        fs.feed(&std::fs::read(&path).unwrap());

        let Some(Folded::SubAgent(sub)) = fs.folded() else {
            panic!("a Sub-agent projection");
        };
        assert_eq!(sub.spawn_key.as_deref(), Some("toolu_a"));
        assert_eq!(sub.errand.as_deref(), Some("map the parser"));
        assert_eq!(sub.depth, 0, "the drifted field alone is lost");
    }

    #[test]
    fn a_sidecar_errand_is_bounded_like_every_other_string_on_the_card() {
        // The same Errand reaches the roster from two places. The parent's copy runs
        // through the same 80-char bound as the activity line; the sidecar's must
        // too, or a row's label length would depend on which side supplied it.
        let dir = tempfile::tempdir().unwrap();
        let path = write_sub_agent(
            dir.path(),
            "a1b2c3",
            meta(&"x".repeat(500), "toolu_a", 1),
            &[sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 1, 1)],
        );
        let fold = claude_fold_at(dir.path(), &path);
        let Some(Folded::SubAgent(sub)) = fold.projection() else {
            panic!("a Sub-agent projection");
        };
        assert_eq!(sub.errand.unwrap().chars().count(), 80);
    }

    #[test]
    fn the_directory_decides_the_root_not_the_files_contents() {
        // A child entry names the root too, and across all 70 real child transcripts
        // it agreed with the containing directory. Reading it therefore decides
        // nothing — but preferring it would let a file's contents move its row onto
        // another session's card, so a disagreement is ignored.
        let dir = tempfile::tempdir().unwrap();
        let path = sub_agent_path(dir.path(), "a1b2c3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let liar = serde_json::json!({
            "type": "assistant", "isSidechain": true, "agentId": "a1b2c3",
            "sessionId": "some-other-session", "cwd": "/Users/x/repos/foo",
            "timestamp": "2026-07-19T10:00:10Z",
            "message": { "model": "claude-haiku-4-5",
                "usage": { "input_tokens": 5, "output_tokens": 1 },
                "content": [{ "type": "text", "text": "sub work" }] }
        });
        std::fs::write(&path, format!("{liar}\n")).unwrap();
        let mut fs = FileState::new(claude_fold_at(dir.path(), &path));
        fs.feed(&std::fs::read(&path).unwrap());

        let Some(Folded::SubAgent(sub)) = fs.folded() else {
            panic!("a Sub-agent projection");
        };
        assert_eq!(sub.attachment, Some(Attachment::Root(ROOT.into())));
    }

    #[test]
    fn a_sub_agent_without_a_readable_sidecar_still_folds() {
        // A sidecar that is missing or drifted costs the row its label, never the
        // row: the Sub-agent still folds, still attaches to its root, and still shows
        // what it spent — unlabelled rather than absent.
        let dir = tempfile::tempdir().unwrap();
        let path = sub_agent_path(dir.path(), "a1b2c3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                sub_agent_assistant("a1b2c3", "claude-haiku-4-5", 40, 4)
            ),
        )
        .unwrap();
        let mut fs = FileState::new(claude_fold_at(dir.path(), &path));
        fs.feed(&std::fs::read(&path).unwrap());

        let Some(Folded::SubAgent(sub)) = fs.folded() else {
            panic!("a Sub-agent transcript states a Sub-agent projection");
        };
        assert_eq!(sub.attachment, Some(Attachment::Root(ROOT.into())));
        assert_eq!(sub.errand, None);
        assert_eq!(sub.spawn_key, None);
        assert_eq!(sub.tokens_in, 40);
    }

    #[test]
    fn an_agent_session_path_still_folds_into_a_card() {
        // The other side of the same classification: a flat transcript under a
        // project directory is an Agent Session, and still becomes a card.
        let mut fs = FileState::new(claude_fold(SESSION_PATH));
        fs.feed(format!("{}\n", assistant("s1", "hello", 10, 2)).as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:20Z"), ts("2026-07-19T10:00:30Z"))
            .unwrap();
        assert_eq!(s.id, "s1");
        assert_eq!(s.tokens_in, 10);
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn assistant(id: &str, text: &str, tin: u64, tout: u64) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"{id}","timestamp":"2026-07-19T10:00:00Z","cwd":"/Users/x/repos/foo","gitBranch":"main","message":{{"model":"claude-opus-4-8","usage":{{"input_tokens":{tin},"output_tokens":{tout}}},"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn sums_tokens_and_takes_latest_fields() {
        let mut fs = claude();
        let mut data = assistant("s1", "first", 100, 10);
        data.push('\n');
        data.push_str(&assistant("s1", "second", 50, 5));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:01:00Z"), ts("2026-07-19T10:02:00Z"))
            .unwrap();
        assert_eq!(s.id, "s1");
        assert_eq!(s.tool, Tool::Claude);
        assert_eq!(s.project, "foo");
        assert_eq!(s.tokens_in, 150);
        assert_eq!(s.tokens_out, 15);
        assert_eq!(s.activity.as_deref(), Some("second"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s.status, Status::Active);
    }

    /// A main-chain assistant turn that spawns a Sub-agent, in the shape Claude Code
    /// writes today: an `Agent` tool-use carrying the correlation id the Sub-agent's
    /// sidecar joins back on, and a `description` that is the Errand.
    fn agent_spawn(tuid: &str, description: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "sessionId": "s1",
            "cwd": "/a/foo",
            "timestamp": "2026-07-19T10:00:00Z",
            "message": {
                "model": "claude-opus-4-8",
                "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use", "id": tuid, "name": "Agent",
                    "input": {
                        "description": description,
                        "subagent_type": "Explore",
                        "prompt": "the long brief the Sub-agent actually receives",
                    }
                }]
            }
        })
        .to_string()
    }

    /// The `tool_result` that answers an `Agent` spawn: a **launch acknowledgement**,
    /// arriving ~2s after the spawn against children that run up to 20 minutes.
    fn agent_launch_ack(tuid: &str) -> String {
        serde_json::json!({
            "type": "user",
            "sessionId": "s1",
            "cwd": "/a/foo",
            "message": { "content": [{
                "type": "tool_result", "tool_use_id": tuid,
                "content": "Async agent launched successfully. You will be notified automatically when it completes.",
            }] }
        })
        .to_string()
    }

    #[test]
    fn the_roster_carries_each_spawn_with_its_errand() {
        // What the parent's own transcript establishes: that a Sub-agent exists and
        // what it was sent to do, in spawn order, from the instant of the spawn.
        let mut fs = claude();
        let mut data = agent_spawn("toolu_a", "map the parser");
        data.push('\n');
        data.push_str(&agent_spawn("toolu_b", "audit the tests"));
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:20Z"), ts("2026-07-19T10:00:30Z"))
            .unwrap();
        assert_eq!(
            s.sub_agent_roster
                .iter()
                .map(|a| (a.errand.as_deref(), a.state))
                .collect::<Vec<_>>(),
            vec![
                (Some("map the parser"), SubAgentState::Running),
                (Some("audit the tests"), SubAgentState::Running),
            ]
        );
        // The join key back to the spawn travels with the entry — it is what the
        // Sub-agent's own file joins onto.
        assert_eq!(s.sub_agent_roster[0].spawn_key, "toolu_a");
        assert_eq!(s.sub_agent_roster[1].spawn_key, "toolu_b");
    }

    #[test]
    fn a_launch_acknowledgement_does_not_end_a_sub_agent() {
        // The fourth breakage, stated as a test. An `Agent` spawn's `tool_result` is
        // the launcher saying it launched — not the Sub-agent saying it finished. The
        // old rule retired the entry here, which would have emptied the badge two
        // seconds after every spawn even with the tool rename fixed.
        let mut fs = claude();
        let mut data = agent_spawn("toolu_a", "map the parser");
        data.push('\n');
        data.push_str(&agent_launch_ack("toolu_a"));
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:20Z"), ts("2026-07-19T10:00:30Z"))
            .unwrap();
        assert_eq!(s.sub_agent_roster.len(), 1);
        assert_eq!(s.sub_agent_roster[0].state, SubAgentState::Running);
        assert_eq!(
            s.sub_agent_roster[0].errand.as_deref(),
            Some("map the parser")
        );
    }

    /// The `<task-notification>` block Claude Code writes: the structured tags that
    /// carry the join key and the outcome word, and the prose beside them that nothing
    /// reads. Every record form below is this same body.
    fn notification_body(tuid: &str, task_id: &str, status: &str) -> String {
        format!(
            "<task-notification>\n<task-id>{task_id}</task-id>\n<tool-use-id>{tuid}</tool-use-id>\n<output-file>/tmp/{task_id}.output</output-file>\n<status>{status}</status>\n<summary>Agent \"map the parser\" finished</summary>\n<note>A task-notification fires each time this agent stops…</note>\n<result>the child's whole report</result>\n</task-notification>"
        )
    }

    /// The completion record itself, as it reaches the **parent's** fold: a user turn
    /// whose whole prompt is the notification.
    fn task_notification(tuid: &str, task_id: &str, status: &str) -> String {
        serde_json::json!({
            "type": "user",
            "sessionId": "s1",
            "cwd": "/a/foo",
            "timestamp": "2026-07-19T10:20:00Z",
            "message": { "role": "user", "content": notification_body(tuid, task_id, status) }
        })
        .to_string()
    }

    /// One `queue-operation` record. The queue states the same notification twice, on
    /// the `enqueue` that parks it and the `remove` that takes it back off.
    fn queue_operation(operation: &str, tuid: &str, task_id: &str, status: &str) -> String {
        serde_json::json!({
            "type": "queue-operation", "operation": operation,
            "timestamp": "2026-07-19T10:20:00Z", "sessionId": "s1",
            "content": notification_body(tuid, task_id, status),
        })
        .to_string()
    }

    /// The same notification's other two record forms — the queue record that
    /// enqueued it and the attachment that carries the queued prompt.
    ///
    /// These are not a duplicate of the user turn above but the **only** forms written
    /// when the Sub-agent finishes while its parent is mid-turn, which is what a
    /// fan-out orchestrator is (issue #85): 33 of 92 spawns on the corpus notified this
    /// way and this way only. So a completion is read out of whichever record carries
    /// it, and reading all three is harmless because the join is by id and latest wins.
    fn task_notification_queue_forms(tuid: &str, task_id: &str, status: &str) -> [String; 2] {
        let body = notification_body(tuid, task_id, status);
        [
            queue_operation("enqueue", tuid, task_id, status),
            serde_json::json!({
                "type": "attachment", "sessionId": "s1", "cwd": "/a/foo",
                "timestamp": "2026-07-19T10:20:00Z",
                "attachment": { "type": "queued_command", "prompt": body,
                    "commandMode": "task-notification" },
            })
            .to_string(),
        ]
    }

    /// A message the orchestrator sends to a Sub-agent that had already finished —
    /// the one way a Sub-agent goes back to work. It names the Sub-agent by the id the
    /// notification stated, and the notification that ends the resumed run carries
    /// *this* tool-use id.
    fn resume_message(tuid: &str, to: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "sessionId": "s1",
            "cwd": "/a/foo",
            "timestamp": "2026-07-19T10:25:00Z",
            "message": {
                "model": "claude-opus-4-8",
                "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use", "id": tuid, "name": "SendMessage",
                    "input": { "to": to, "summary": "finish it yourself now",
                        "message": "the long message the Sub-agent receives" }
                }]
            }
        })
        .to_string()
    }

    /// Feed `lines` to a fresh Claude fold and build the card at a fixed clock.
    fn fold_lines(lines: &[String]) -> Session {
        let mut fs = claude();
        let mut data = lines.join("\n");
        data.push('\n');
        fs.feed(data.as_bytes());
        fs.build(ts("2026-07-19T10:30:00Z"), ts("2026-07-19T10:30:10Z"))
            .unwrap()
    }

    /// Each roster row's state and the source's word for how it ended.
    fn outcomes(s: &Session) -> Vec<(SubAgentState, Option<&str>)> {
        s.sub_agent_roster
            .iter()
            .map(|a| (a.state, a.outcome.as_deref()))
            .collect()
    }

    #[test]
    fn a_completion_notification_finishes_its_entry_while_its_parent_runs() {
        // The ticket, stated once: a roster entry stops being Running the moment its
        // Sub-agent actually finishes — which its parent's notification says, ~20
        // minutes after the launch acknowledgement — and says so in the source's own
        // word. The parent is still Running; the Sub-agent it has not heard about is
        // still Running too.
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            agent_spawn("toolu_b", "audit the tests"),
            agent_launch_ack("toolu_a"),
            task_notification("toolu_a", "task-a", "completed"),
        ]);
        assert_eq!(
            outcomes(&s),
            vec![
                (SubAgentState::Finished, Some("completed")),
                (SubAgentState::Running, None),
            ]
        );
        assert_eq!(s.status, Status::Active);
    }

    #[test]
    fn every_word_the_source_states_travels_verbatim() {
        // The vocabulary is the source's, not ours: `completed`, `failed`, `stopped`,
        // `killed` are read off the status tag, never inferred from the prose beside
        // it — and a word we have never seen would travel just as unaltered.
        for word in ["completed", "failed", "stopped", "killed", "abandoned"] {
            let s = fold_lines(&[
                agent_spawn("toolu_a", "map the parser"),
                task_notification("toolu_a", "task-a", word),
            ]);
            assert_eq!(outcomes(&s), vec![(SubAgentState::Finished, Some(word))]);
        }
    }

    #[test]
    fn the_latest_notification_wins() {
        // A Sub-agent can notify more than once, so the roster shows the latest word
        // rather than treating the first as final.
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            task_notification("toolu_a", "task-a", "completed"),
            task_notification("toolu_a", "task-a", "failed"),
        ]);
        assert_eq!(
            outcomes(&s),
            vec![(SubAgentState::Finished, Some("failed"))]
        );
    }

    #[test]
    fn a_resumed_sub_agent_goes_back_to_running() {
        // How a Sub-agent ends is the latest word, not a final one. The orchestrator
        // sends a finished Sub-agent another message; the roster says what is true now
        // — Running, with no outcome — and the notification that ends the resumed run
        // arrives under the *resuming* tool-use id, which is the one the source states.
        let resumed = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            task_notification("toolu_a", "task-a", "completed"),
            resume_message("toolu_resume", "task-a"),
        ]);
        assert_eq!(outcomes(&resumed), vec![(SubAgentState::Running, None)]);
        assert_eq!(resumed.sub_agent_roster.len(), 1, "still one Sub-agent");

        let ended_again = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            task_notification("toolu_a", "task-a", "completed"),
            resume_message("toolu_resume", "task-a"),
            task_notification("toolu_resume", "task-a", "stopped"),
        ]);
        assert_eq!(
            outcomes(&ended_again),
            vec![(SubAgentState::Finished, Some("stopped"))]
        );
        assert_eq!(ended_again.sub_agent_roster.len(), 1);
    }

    #[test]
    fn a_notification_joins_a_spawn_by_id_and_is_never_counted() {
        // 101 task-ids appear against 59 spawns: notifications are not Sub-agent
        // specific — a backgrounded command notifies too. One that names a tool-use
        // that is not a spawn moves nothing and creates nothing; counting them would
        // attribute a shell command to a Sub-agent.
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            task_notification("toolu_bash", "task-bg", "failed"),
        ]);
        assert_eq!(outcomes(&s), vec![(SubAgentState::Running, None)]);
        assert_eq!(
            s.sub_agent_roster.len(),
            1,
            "no row for a background command"
        );
    }

    #[test]
    fn a_notification_that_states_no_status_ends_nothing() {
        // Monitor events notify under the same tag with no status and no tool-use id.
        // How a Sub-agent ended is read or it is absent — never inferred from the
        // summary prose sitting right beside it.
        let mut fs = claude();
        let monitor = serde_json::json!({
            "type": "user", "sessionId": "s1", "cwd": "/a/foo",
            "timestamp": "2026-07-19T10:20:00Z",
            "message": { "role": "user", "content":
                "<task-notification>\n<task-id>bi9juvjcz</task-id>\n<summary>Monitor event: \"map the parser\"</summary>\n<event>the agent failed and died</event>\n</task-notification>" }
        })
        .to_string();
        let mut data = [agent_spawn("toolu_a", "map the parser"), monitor].join("\n");
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:30:00Z"), ts("2026-07-19T10:30:10Z"))
            .unwrap();
        assert_eq!(outcomes(&s), vec![(SubAgentState::Running, None)]);
    }

    #[test]
    fn a_notification_in_all_its_record_forms_produces_one_outcome() {
        // The same notification can be written three times — a queue record, a queued-
        // command attachment, and the user turn. All three are read, and the roster
        // still carries one Sub-agent with one outcome: the join is by tool-use id, so
        // the repeats land on the same row and the last simply restates the word.
        let [queued, attached] = task_notification_queue_forms("toolu_a", "task-a", "completed");
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            queued.clone(),
            attached.clone(),
            task_notification("toolu_a", "task-a", "completed"),
        ]);
        assert_eq!(
            outcomes(&s),
            vec![(SubAgentState::Finished, Some("completed"))]
        );
        assert_eq!(s.sub_agent_roster.len(), 1, "one Sub-agent, not three");
    }

    #[test]
    fn a_completion_is_read_from_whichever_record_carries_it() {
        // Issue #85. The user turn is written only when the parent is *idle* when its
        // child ends. A parent that is mid-turn — which is what an orchestrator that
        // just fanned out is — gets the notification enqueued instead, and the only
        // records ever written are these two. Dropping them left 33 of 92 spawns on
        // the corpus Running forever, and `has_active_sub_agents` then pinned 20 of 41
        // fan-out parents Active for as long as the process lived.
        let [queued, attached] = task_notification_queue_forms("toolu_a", "task-a", "completed");

        // Each carrier alone is the completion, and carries the source's own word.
        for carrier in [&queued, &attached] {
            let s = fold_lines(&[agent_spawn("toolu_a", "map the parser"), carrier.clone()]);
            assert_eq!(
                outcomes(&s),
                vec![(SubAgentState::Finished, Some("completed"))],
                "a queued notification is still the only statement of how it ended"
            );
        }

        // The `remove` that dequeues it restates the same record; latest-wins makes
        // that idempotent rather than a second ending.
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            queued.clone(),
            queue_operation("remove", "toolu_a", "task-a", "completed"),
        ]);
        assert_eq!(
            outcomes(&s),
            vec![(SubAgentState::Finished, Some("completed"))]
        );

        // Join by id, never by count, holds for the queued forms too: a backgrounded
        // command notifies under the same tag and must move nothing.
        let [other, _] = task_notification_queue_forms("toolu_bash", "task-bg", "completed");
        let unrelated = fold_lines(&[agent_spawn("toolu_a", "map the parser"), other]);
        assert_eq!(outcomes(&unrelated), vec![(SubAgentState::Running, None)]);
        assert_eq!(unrelated.sub_agent_roster.len(), 1);
    }

    #[test]
    fn a_queued_notification_lets_a_quiet_parent_finish() {
        // The harm #85 actually did, stated as a test. Dropping the queued forms did
        // not merely mislabel a row: a row claiming to be Running is what holds a quiet
        // parent out of Finished, so the card stayed Active for as long as the process
        // lived. One session on the corpus was still served `active` five hours after
        // everything on it had stopped.
        let mut fs = claude();
        let mut data = agent_spawn("toolu_a", "map the parser");
        data.push('\n');
        fs.feed(data.as_bytes());
        // Eight hours quiet, with the Sub-agent still Running: the parent is working,
        // because a fan-out parent genuinely goes silent while its children run.
        let mtime = ts("2026-07-19T10:20:00Z");
        let much_later = ts("2026-07-19T18:00:00Z");
        let working = fs.build(mtime, much_later).unwrap();
        assert_eq!(working.status, Status::Active);
        assert_eq!(outcomes(&working), vec![(SubAgentState::Running, None)]);

        // The child ends while the parent is mid-turn, so only the queued records are
        // written. Reading them is what lets the card go Finished.
        let [queued, attached] = task_notification_queue_forms("toolu_a", "task-a", "completed");
        let mut more = queued;
        more.push('\n');
        more.push_str(&attached);
        more.push('\n');
        fs.feed(more.as_bytes());
        let done = fs.build(mtime, much_later).unwrap();
        assert_eq!(
            outcomes(&done),
            vec![(SubAgentState::Finished, Some("completed"))]
        );
        assert_eq!(
            done.status,
            Status::Finished,
            "nothing is running any more, so nothing should hold the card Active"
        );
    }

    #[test]
    fn a_non_turn_record_carrying_no_completion_is_an_absence_not_a_failure() {
        // Reading a record type that is not a turn must keep the distinction the fold
        // depends on: `Ok(None)` is "nothing here", `Err` is "this line is a mid-write
        // fragment, come back to it". A queued payload of an unexpected shape is the
        // first, or a transcript would stall on its own bookkeeping records.
        for line in [
            r#"{"type":"queue-operation","operation":"enqueue","content":"just a prompt someone typed"}"#,
            r#"{"type":"queue-operation","operation":"enqueue","content":{"not":"a string"}}"#,
            r#"{"type":"queue-operation","operation":"enqueue","content":null}"#,
            r#"{"type":"attachment","attachment":{"type":"file","path":"/a/b.rs"}}"#,
            r#"{"type":"summary","summary":"a compacted conversation"}"#,
        ] {
            assert!(
                matches!(crate::parse::parse_entry(line), Ok(None)),
                "{line} should read as nothing, not as a malformed line"
            );
        }
    }

    #[test]
    fn a_queued_notification_moves_a_roster_row_and_nothing_else() {
        // It is still an agent-level event addressed to the orchestrator, so reading it
        // out of a new record type must not hand the card anything else: no tokens of
        // its own, no activity line, and neither raising a human need nor answering one
        // (ADR 0010). The Bash call the person is waiting on is still waiting.
        let [queued, attached] = task_notification_queue_forms("toolu_a", "task-a", "failed");
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            assistant("s1", "reading the parser", 100, 10),
            assistant_tool_use("s1"),
            queued,
            attached,
        ]);
        assert_eq!(
            outcomes(&s),
            vec![(SubAgentState::Finished, Some("failed"))]
        );
        assert_eq!((s.tokens_in, s.tokens_out), (100, 10));
        assert_eq!(s.activity.as_deref(), Some("reading the parser"));
        assert_eq!(s.status, Status::Attention);
        let a = s
            .attention
            .expect("the human's wait survives a queued notification");
        assert_eq!(a.cause, AttentionCause::Input);
        assert_eq!(a.evidence.as_deref(), Some("Bash"));
    }

    #[test]
    fn a_failed_sub_agent_leaves_the_parents_attention_exactly_as_it_was() {
        // The notification is addressed to the orchestrator, which reads it and
        // carries on — in 23 of 24 observed non-completed notifications the parent's
        // next entry came a median of 0.7s later. It manufactures no human need
        // (ADR 0010), and it answers none either: a person is still waiting to approve
        // that Bash call, and the card must not stop saying so.
        let s = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            assistant_tool_use("s1"),
            task_notification("toolu_a", "task-a", "failed"),
        ]);
        assert_eq!(
            outcomes(&s),
            vec![(SubAgentState::Finished, Some("failed"))]
        );
        assert_eq!(s.status, Status::Attention);
        let a = s
            .attention
            .expect("the human's wait survives the notification");
        assert_eq!(a.cause, AttentionCause::Input);
        assert_eq!(a.since, ts("2026-07-19T10:00:00Z"));
        assert_eq!(a.evidence.as_deref(), Some("Bash"));

        // A notification arriving in the same turn as the answer to that Bash call
        // still lets the answer through: the two records are unrelated, and reading one
        // must not cost the other. Every notification observed arrived alone, so this
        // is the shape that would rot silently — the card stuck in Attention with the
        // tool it named already answered.
        let answered = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            assistant_tool_use("s1"),
            serde_json::json!({
                "type": "user", "sessionId": "s1", "cwd": "/a/foo",
                "timestamp": "2026-07-19T10:20:00Z",
                "message": { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok" },
                    { "type": "text", "text": notification_body("toolu_a", "task-a", "failed") },
                ] }
            })
            .to_string(),
        ]);
        assert_eq!(
            outcomes(&answered),
            vec![(SubAgentState::Finished, Some("failed"))]
        );
        assert_eq!(
            answered.status,
            Status::Active,
            "the Bash call was answered"
        );
        assert!(answered.attention.is_none());

        // And with nothing pending, a failure raises nothing of its own.
        let quiet = fold_lines(&[
            agent_spawn("toolu_a", "map the parser"),
            task_notification("toolu_a", "task-a", "failed"),
        ]);
        assert_eq!(quiet.status, Status::Active);
        assert!(quiet.attention.is_none());
    }

    #[test]
    fn a_spawn_that_names_no_purpose_carries_no_errand() {
        // Errand is absent rather than substituted: a Sub-agent with no stated
        // purpose is shown unlabelled, never labelled with something that merely
        // looks like one.
        let mut fs = claude();
        let line = serde_json::json!({
            "type": "assistant",
            "sessionId": "s1",
            "cwd": "/a/foo",
            "timestamp": "2026-07-19T10:00:00Z",
            "message": {
                "model": "claude-opus-4-8",
                "stop_reason": "tool_use",
                "content": [{
                    "type": "tool_use", "id": "toolu_x", "name": "Agent",
                    "input": { "subagent_type": "Explore" }
                }]
            }
        });
        fs.feed(format!("{line}\n").as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:20Z"), ts("2026-07-19T10:00:30Z"))
            .unwrap();
        assert_eq!(s.sub_agent_roster.len(), 1);
        assert!(s.sub_agent_roster[0].errand.is_none());
    }

    #[test]
    fn spawning_a_sub_agent_is_working_not_attention_or_stale() {
        // An `Agent` spawn ends the turn with stop_reason tool_use, but it is the
        // agent fanning out — not a human-input wait — so the card must not enter
        // Attention. And with a Sub-agent still Running the parent stays Active even
        // when its own transcript has been quiet well past the staleness window: the
        // observed quiet spans reach 963 seconds.
        let mut fs = claude();
        fs.feed(format!("{}\n", agent_spawn("toolu_a", "grind on it")).as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:00Z"), ts("2026-07-19T10:30:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Active);
        assert!(s.attention.is_none());
        assert_eq!(s.sub_agent_roster.len(), 1);
        assert_eq!(s.sub_agent_roster[0].state, SubAgentState::Running);
    }

    #[test]
    fn a_sidechain_turn_in_a_parent_transcript_is_ignored_not_folded() {
        // Today's Claude Code writes zero of these into a parent transcript — the
        // traffic moved into the Sub-agent's own file. A legacy transcript still
        // inside the discovery window must not have that turn folded as the parent's
        // own: not its tokens, not its words as the activity line, and above all not
        // its tool call as a human wait (ADR 0010).
        let mut fs = claude();
        let mut data = assistant("s1", "orchestrating", 100, 10);
        data.push('\n');
        data.push_str(
            &serde_json::json!({
                "type": "assistant", "isSidechain": true, "sessionId": "s1",
                "cwd": "/a/foo", "timestamp": "2026-07-19T10:00:10Z",
                "message": { "model": "claude-haiku-4-5", "stop_reason": "tool_use",
                    "usage": { "input_tokens": 999, "output_tokens": 99 },
                    "content": [
                        { "type": "text", "text": "a Sub-agent's own words" },
                        { "type": "tool_use", "id": "toolu_sub", "name": "Bash" }
                    ] }
            })
            .to_string(),
        );
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(
            s.tokens_in, 100,
            "the Sub-agent's spend is not the parent's"
        );
        assert_eq!(s.activity.as_deref(), Some("orchestrating"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s.status, Status::Active);
        assert!(
            s.attention.is_none(),
            "a Sub-agent's tool call is not a human wait"
        );
        // Identity is still assigned: the guard sits after it, never above it.
        assert_eq!(s.id, "s1");
    }

    #[test]
    fn a_parent_transcript_carries_no_sub_agent_spend_of_its_own() {
        // Sub-agent turns left the parent transcript — parent-side sidechain entries
        // number zero across the whole corpus. A parent's own counts are its own; its
        // Sub-agents' spend arrives from their files, joined by the store.
        let mut fs = claude();
        fs.feed(format!("{}\n", assistant("s1", "orchestrating", 100, 10)).as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(s.tokens_in, 100);
        assert_eq!(s.tokens_out, 10);
        assert_eq!(s.activity.as_deref(), Some("orchestrating"));
    }

    #[test]
    fn pending_tool_use_is_attention() {
        let mut fs = claude();
        let line = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{"model":"m","content":[{"type":"tool_use","id":"toolu_1","name":"Bash"}]}}"#;
        let mut data = line.to_string();
        data.push('\n');
        fs.feed(data.as_bytes());

        let now = ts("2026-07-19T10:05:00Z");
        let s = fs.build(ts("2026-07-19T10:04:00Z"), now).unwrap();
        assert_eq!(s.status, Status::Attention);
    }

    #[test]
    fn answered_tool_use_is_active_not_attention() {
        let mut fs = claude();
        let assistant_tu = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","message":{"model":"m","content":[{"type":"tool_use","id":"toolu_1","name":"Bash"}]}}"#;
        let user_tr = r#"{"type":"user","sessionId":"s1","cwd":"/a/foo","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}"#;
        let mut data = assistant_tu.to_string();
        data.push('\n');
        data.push_str(user_tr);
        data.push('\n');
        fs.feed(data.as_bytes());

        let now = ts("2026-07-19T10:05:00Z");
        let s = fs.build(ts("2026-07-19T10:04:00Z"), now).unwrap();
        assert_eq!(s.status, Status::Active);
    }

    /// An assistant turn that ended to call a tool, carrying an explicit
    /// `stop_reason: tool_use`.
    fn assistant_tool_use(id: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"{id}","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{{"model":"m","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_1","name":"Bash"}}]}}}}"#
        )
    }

    #[test]
    fn stop_reason_tool_use_is_attention_waiting() {
        let mut fs = claude();
        let mut data = assistant_tool_use("s1");
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Attention);
        assert_eq!(cause(&s), Some(AttentionCause::Input));
    }

    #[test]
    fn end_turn_with_a_tool_use_block_is_not_attention() {
        // stop_reason wins over the mere presence of a tool_use block: a turn that
        // ended cleanly is not a wait, even if it references a tool.
        let mut fs = claude();
        let line = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","message":{"model":"m","stop_reason":"end_turn","content":[{"type":"tool_use","id":"t1","name":"Bash"},{"type":"text","text":"done"}]}}"#;
        let mut data = line.to_string();
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Active);
        assert_eq!(cause(&s), None);
    }

    #[test]
    fn api_error_is_attention_error_and_preserves_model() {
        let mut fs = claude();
        // A real assistant turn, then a synthetic API-error record.
        let mut data = assistant("s1", "hello", 10, 2);
        data.push('\n');
        data.push_str(r#"{"type":"assistant","isApiErrorMessage":true,"sessionId":"s1","cwd":"/a/foo","message":{"model":"<synthetic>","stop_reason":"stop_sequence","content":[{"type":"text","text":"API Error: overloaded"}]}}"#);
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Attention);
        assert_eq!(cause(&s), Some(AttentionCause::Error));
        // The synthetic record must not clobber the card's real model/activity.
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s.activity.as_deref(), Some("hello"));
    }

    #[test]
    fn activity_after_api_error_clears_attention() {
        // Recovery (story 19): a fresh turn after an error leaves the error state.
        let mut fs = claude();
        let mut data =
            r#"{"type":"assistant","isApiErrorMessage":true,"sessionId":"s1","cwd":"/a/foo","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error"}]}}"#
                .to_string();
        data.push('\n');
        data.push_str(&assistant("s1", "resumed", 5, 1));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Active);
        assert_eq!(cause(&s), None);
    }

    #[test]
    fn malformed_nonfinal_line_is_skipped() {
        let mut fs = claude();
        let mut data = String::from("{ this is not json }\n");
        data.push_str(&assistant("s1", "after", 3, 2));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(s.id, "s1");
        assert_eq!(s.tokens_in, 3); // survived the bad line
    }

    #[test]
    fn truncated_final_line_is_retried_not_consumed() {
        let mut fs = claude();
        // A complete line then a half-written one (no trailing newline).
        let mut data = assistant("s1", "one", 4, 1);
        data.push('\n');
        let complete_len = data.len() as u64;
        data.push_str(r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","messa"#);
        fs.feed(data.as_bytes());

        // Offset stops at the end of the complete line; the fragment is deferred.
        assert_eq!(fs.offset(), complete_len);
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
        assert_eq!(s.tokens_in, 4);

        // The rest of the line arrives; feeding from the retained offset completes it.
        let full = {
            let mut d = assistant("s1", "one", 4, 1);
            d.push('\n');
            d.push_str(&assistant("s1", "two", 6, 2));
            d.push('\n');
            d
        };
        fs.feed(&full.as_bytes()[fs.offset() as usize..]);
        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(s.tokens_in, 10);
        assert_eq!(s.activity.as_deref(), Some("two"));
    }

    #[test]
    fn truncation_below_offset_resets() {
        let mut fs = claude();
        let mut data = assistant("s1", "one", 100, 10);
        data.push('\n');
        fs.feed(data.as_bytes());
        assert_eq!(
            fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
                .unwrap()
                .tokens_in,
            100
        );

        // Simulate a rewrite shorter than our offset.
        fs.reset();
        let mut rewritten = assistant("s1", "fresh", 7, 3);
        rewritten.push('\n');
        fs.feed(rewritten.as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(s.tokens_in, 7); // not 107
    }

    #[test]
    fn no_session_id_yields_no_card() {
        let mut fs = claude();
        let line = r#"{"type":"assistant","cwd":"/a/foo","message":{"model":"m","content":[]}}"#;
        let mut data = line.to_string();
        data.push('\n');
        fs.feed(data.as_bytes());
        assert!(fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .is_none());
    }

    #[test]
    fn pending_tool_use_carries_input_cause_and_local_evidence() {
        // A tool the agent is blocked on: honest generic cause, source-faithful
        // local evidence (tool name + a recognized input field), Since = the call's
        // own timestamp.
        let mut fs = claude();
        let line = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"cargo test --workspace"}}]}}"#;
        let mut data = line.to_string();
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        let a = s.attention.unwrap();
        assert_eq!(a.cause, AttentionCause::Input);
        assert_eq!(a.since, ts("2026-07-19T10:00:00Z"));
        assert_eq!(a.evidence.as_deref(), Some("Bash: cargo test --workspace"));
        // The wire rendering keeps the tool name but never its arguments.
        assert_eq!(a.remote_evidence.as_deref(), Some("Bash"));
    }

    #[test]
    fn a_newer_need_replaces_the_current_one_and_resets_since() {
        // Two different pending tool calls in sequence: the card describes the
        // current need, and Attention Since jumps to the newer call (Story 8).
        let mut fs = claude();
        let first = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_1","name":"Read"}]}}"#;
        let result = r#"{"type":"user","sessionId":"s1","cwd":"/a/foo","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}"#;
        let second = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:03:00Z","message":{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_2","name":"Bash"}]}}"#;
        let mut data = String::new();
        for l in [first, result, second] {
            data.push_str(l);
            data.push('\n');
        }
        fs.feed(data.as_bytes());

        let a = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap()
            .attention
            .unwrap();
        assert_eq!(a.since, ts("2026-07-19T10:03:00Z"));
        assert_eq!(a.evidence.as_deref(), Some("Bash"));
    }

    #[test]
    fn string_content_does_not_break_parsing() {
        let mut fs = claude();
        // Some user turns carry `content` as a bare string.
        let line = r#"{"type":"user","sessionId":"s1","cwd":"/a/foo","message":{"content":"hello there"}}"#;
        let mut data = line.to_string();
        data.push('\n');
        data.push_str(&assistant("s1", "reply", 2, 1));
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
        assert_eq!(s.activity.as_deref(), Some("reply"));
    }
}
