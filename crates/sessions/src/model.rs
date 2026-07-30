//! The Session model: what the board renders as a card, serialized to the UI as
//! camelCase JSON.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The agent tool a Session came from. One Session Source per tool; carried to
/// the UI so each card can show the right tool tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Claude,
    Codex,
}

/// Board status for an Agent Session. The flat three-value shape keeps the client
/// simple; `Attention` carries its structured cause and evidence in
/// [`Session::attention`]. See CONTEXT.md, issue #2 (mtime-based Active↔Finished)
/// and ADR 0010 (typed, source-agnostic Attention lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Touched within the activity window and not needing a human.
    Active,
    /// The session needs a human: it is waiting on input (approval, question,
    /// review) or ended in an error. The typed cause is in [`Session::attention`].
    /// Outranks staleness, so an old-but-unanswered wait stays here rather than
    /// aging into `Finished`.
    Attention,
    /// Untouched for at least the activity window and not needing a human.
    Finished,
}

/// The typed kind of human response an Agent Session requires (ADR 0010,
/// CONTEXT.md "Attention Cause"). A closed enum: a specific cause comes only from
/// structured source evidence, never inference from prose. When a source's records
/// support nothing more specific, the nonspecific fallback [`Input`](Self::Input)
/// is used — an honest generic cause rather than an invented one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionCause {
    /// A pending approval the human must grant or deny (e.g. Codex exec /
    /// apply-patch approval).
    Approval,
    /// The agent asked a question and is waiting for the human's answer.
    Answer,
    /// The agent is waiting on the human to review proposed work.
    Review,
    /// The session ended in an error (an abnormal / aborted ending).
    Error,
    /// Nonspecific fallback: the session needs the human, but the source's records
    /// do not identify a more specific cause.
    Input,
}

/// One current, structured Attention on a Session (ADR 0010). At most one is
/// carried at a time; a newer structured need replaces it and resets [`since`](Self::since).
///
/// `evidence` is the **local display evidence**: a bounded, source-faithful,
/// sanitized excerpt (CONTEXT.md "Attention Evidence"), or `None` when no safe
/// excerpt is available. It is the only evidence serialized to a board — a board
/// receives, per session, either its own machine's local evidence (a local card)
/// or the privacy-safe remote rendering baked in upstream (a relayed card).
///
/// `remote_evidence` is the stricter allowlisted rendering (CONTEXT.md "Remote
/// Attention Evidence") the Collector projects onto the wire; it is held in-process
/// only and never serialized to a board (`#[serde(skip)]`), so rich local evidence
/// cannot leak through the browser JSON. `details_on_source` marks a relayed card
/// whose allowlisted fields could not explain the need: the UI then says details
/// are available only on the source machine rather than showing a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    /// The typed kind of response required.
    pub cause: AttentionCause,
    /// When the current need began (CONTEXT.md "Attention Since"). Drives the
    /// oldest-waiting-first order; reset only when a newer need replaces this one.
    pub since: DateTime<Utc>,
    /// Bounded, sanitized, source-faithful local display evidence, or `None`.
    #[serde(default)]
    pub evidence: Option<String>,
    /// A relayed card whose allowlisted fields could not explain the need — the UI
    /// points at the source machine instead of inventing an explanation.
    #[serde(default)]
    pub details_on_source: bool,
    /// The allowlisted-structured-fields rendering for the wire (CONTEXT.md "Remote
    /// Attention Evidence"). In-process only: never serialized to a board.
    #[serde(skip)]
    pub remote_evidence: Option<String>,
}

/// Lines added / removed in a session's repo — the card's `+/-` stat (C5). Live
/// git working-tree state, not transcript-derived: filled in by the board (see
/// `sessions::git::diff_stat`), so the sessions projection leaves it `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub added: u64,
    pub removed: u64,
}

/// Whether a Sub-agent is still running or has finished.
///
/// A state on each roster entry rather than a property of the collection: a parent
/// carries every Sub-agent it has spawned, and a finished Agent Session by
/// definition has none running — carrying only the active set would show the
/// sessions that delegated the most work nothing at all (ADR 0014).
/// `Finished` is the default a partial wire row degrades to, deliberately: a row we
/// could not fully read must not claim to be running, since a Running row keeps its
/// parent's card out of Finished and would pin a stale session Active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubAgentState {
    Running,
    #[default]
    Finished,
}

/// One Sub-agent on its parent's roster (CONTEXT.md "Sub-agent"). Never a card of
/// its own — it carries no independent human need, since nobody can approve,
/// answer, or resume it directly, only the Agent Session that sent it (ADR 0014).
///
/// Every Sub-agent an Agent Session has spawned stays on that session's roster,
/// running and finished alike, in spawn order. However deep it was spawned, it
/// belongs to the **root** Agent Session — the only node in the spawn tree that is
/// a card.
///
/// `PartialEq` only (not `Eq`): `cost_usd` is an `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubAgent {
    /// The Sub-agent's own source-native id: Claude's `agentId`, Codex's rollout id.
    ///
    /// **Provisional until the Sub-agent's own file is discovered.** A row the parent
    /// alone established carries the spawn key here, because the parent's transcript
    /// names no other id; it becomes the Sub-agent's own once its file is found. Use
    /// [`spawn_key`](Self::spawn_key) for anything that must stay stable across that
    /// transition — a rendering key, say.
    #[serde(default)]
    pub id: String,
    /// The key that joins this entry back to its spawn on the parent's side:
    /// Claude's spawning tool-use id, Codex's parent thread id. It is what lets the
    /// roster be the union of what the parent recorded and what the child spent, and
    /// it is the one identity that does not change as the two sides come together.
    #[serde(default)]
    pub spawn_key: String,
    /// What this Sub-agent was sent to do (CONTEXT.md "Errand"), verbatim from the
    /// spawning source. `None` when the source names no purpose — a Sub-agent with
    /// no Errand is shown unlabelled rather than labelled with something that merely
    /// looks like a purpose.
    #[serde(default)]
    pub errand: Option<String>,
    /// Every field on this row carries `serde(default)`: a roster row is a *badge*,
    /// and a peer across a version skew that omits one must cost the badge rather
    /// than the whole session. Without it a single missing field fails the enclosing
    /// `Session` decode and the Relay drops the card entirely — the exact failure the
    /// wire's field rename was chosen to avoid, which the roster's contents reopen
    /// now that they are no longer always empty.
    #[serde(default)]
    pub state: SubAgentState,
    /// How it ended, in the source's own word (`completed`, `failed`, `stopped`,
    /// `killed`). Read, never inferred from prose, and `None` when the source states
    /// none — including for every Running entry. A Sub-agent can resume after
    /// finishing, so this is the latest word rather than a final one.
    #[serde(default)]
    pub outcome: Option<String>,
    /// This Sub-agent's own token usage. Also counted in its parent's headline
    /// totals; the roster row is the disclosure of whose spend it was.
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    /// Estimated cost priced at [`model`](Self::model) — the Sub-agent's own, which
    /// may be cheaper than the orchestrator's. `None` for an unpriced model.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// The model this Sub-agent ran.
    #[serde(default)]
    pub model: Option<String>,
    /// How deep it was spawned (a Sub-agent may spawn its own). Carried and drawn
    /// nowhere: only 5 of 135 observed Sub-agents nest at all, to a maximum depth of
    /// 3, so carrying it keeps the nesting tree a later rendering change rather than
    /// a re-fold (ADR 0014).
    #[serde(default)]
    pub depth: u32,
    /// This Sub-agent's own latest entry timestamp, or `None` when no timestamped
    /// line has arrived.
    #[serde(default)]
    pub last_event_at: Option<DateTime<Utc>>,
}

/// One Agent Session — a single Claude Code transcript, projected for the UI.
///
/// `PartialEq` only (not `Eq`): `cost_usd` is an `f64`. The store compares
/// successive projections with `!=` to suppress no-op events; cost is deterministic
/// from tokens + model, so equal projections stay equal (no float churn).
///
/// `Deserialize` as well as `Serialize` (C7): the same camelCase shape the board
/// serves to the UI is the wire currency the Collector pushes to the Relay and the
/// Relay fans out to a subscribing board. `diff` and `machine` are the enrichment
/// fields a pre-C7 (or enrichment-less) sender may omit, so both `#[serde(default)]`
/// to `None` — keeping the wire additive in both directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// `sessionId` (uuid). Globally unique, so duplicate filename stems across
    /// project dirs cannot collide.
    pub id: String,
    /// Which agent tool produced this Session.
    pub tool: Tool,
    /// Last path segment of the latest `cwd`.
    pub project: String,
    /// `message.model` of the latest assistant entry.
    pub model: Option<String>,
    /// `gitBranch` of the latest entry.
    pub branch: Option<String>,
    pub cwd: Option<String>,
    /// Sum of assistant `message.usage.input_tokens`.
    pub tokens_in: u64,
    /// Sum of assistant `message.usage.output_tokens`.
    pub tokens_out: u64,
    /// Latest assistant text block, first line, truncated to 80 chars.
    pub activity: Option<String>,
    /// Latest entry timestamp (what the UI sorts a column by).
    pub last_event_at: DateTime<Utc>,
    pub status: Status,
    /// The current, structured Attention, when `status == Attention`; `None`
    /// otherwise. One atomic value (cause + since + evidence) so board status stays
    /// the existing attention-plus-staleness rule and the UI never re-derives
    /// blocked-ness from raw fields. Omitted-on-wire tolerant (`default` → `None`).
    #[serde(default)]
    pub attention: Option<Attention>,
    /// Estimated USD cost from tokens × the model's public list price (C5). `None`
    /// for an unpriced/unknown model. A labelled *estimate*: the UI can hide it for
    /// subscription sessions, which pay no marginal per-token cost.
    pub cost_usd: Option<f64>,
    /// Lines added / removed in the session's repo (C5). Live git state, so the
    /// sessions projection leaves it `None`; whichever process owns the repo (the
    /// board for local sessions, the Collector for remote ones) fills it before
    /// serving/streaming. Omitted-on-wire tolerant (`default` → `None`).
    #[serde(default)]
    pub diff: Option<DiffStat>,
    /// Every Sub-agent this Session has spawned, running and finished alike — the
    /// card's Sub-agent badge and the roster its panel shows. Empty for a session that
    /// never fanned out.
    ///
    /// In spawn order where the source records the spawns, which is Claude. Codex
    /// records none in the parent's rollout, so its rows are ordered by latest activity
    /// instead — an approximation of spawn order, bounded and explained at
    /// [`merge_roster`](crate::fold::merge_roster).
    ///
    /// A **new** wire field name, replacing the legacy count-and-descriptions
    /// object. An absent field defaults cleanly to an empty roster, but a legacy
    /// object arriving where an array is expected is a deserialization error, not a
    /// default — and would cost the whole session rather than just the badge. Under
    /// a new name the legacy field is simply unknown and dropped, which is the
    /// degradation ADR 0014 intends. There is no legacy content to preserve: the
    /// tool-name bug means every legacy Collector has only ever sent an empty one.
    #[serde(default)]
    pub sub_agent_roster: Vec<SubAgent>,
    /// The machine this Session runs on — the host's name (C7). Stamped at the
    /// source: the board's own local runtime (and, later, a Collector on a remote
    /// machine) sets it to the local hostname before the Session leaves the watcher,
    /// so every card can show which machine an Agent Session is on. Like `diff`, the
    /// sessions projection leaves it `None`; a `None` still serializes cleanly,
    /// keeping the field additive on the wire for local-only, pre-C7 boards.
    #[serde(default)]
    pub machine: Option<String>,
}
