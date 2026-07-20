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
/// simple; `Attention` carries its cause in [`Session::attention_reason`]. See
/// CONTEXT.md, issue #2 (mtime-based Active↔Finished) and issue #7 (principled,
/// source-agnostic Attention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Touched within the activity window and not needing a human.
    Active,
    /// The session needs a human: it is waiting on input or ended in an error.
    /// The cause is in [`Session::attention_reason`]. Outranks staleness, so an
    /// old-but-unanswered wait stays here rather than aging into `Finished`.
    Attention,
    /// Untouched for at least the activity window and not needing a human.
    Finished,
}

/// Why a Session is in [`Status::Attention`] — the two, and only two, causes the
/// glossary allows. Serialized alongside `status` and populated only when the
/// status is `Attention`. Staleness is never a cause (it is a card hint only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionReason {
    /// Waiting on a human — a pending approval or question.
    Waiting,
    /// The session ended in an error (an abnormal / aborted ending).
    Error,
}

/// Lines added / removed in a session's repo — the card's `+/-` stat (C5). Live
/// git working-tree state, not transcript-derived: filled in by the board (see
/// `collector::git::diff_stat`), so the collector always leaves it `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub added: u64,
    pub removed: u64,
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
    /// Why the session needs a human, when `status == Attention`; `None` otherwise.
    /// Explicit and typed so the UI never re-derives blocked-ness from raw fields.
    pub attention_reason: Option<AttentionReason>,
    /// Estimated USD cost from tokens × the model's public list price (C5). `None`
    /// for an unpriced/unknown model. A labelled *estimate*: the UI can hide it for
    /// subscription sessions, which pay no marginal per-token cost.
    pub cost_usd: Option<f64>,
    /// Lines added / removed in the session's repo (C5). Live git state, so the
    /// collector *projection* leaves it `None`; whichever process owns the repo (the
    /// board for local sessions, the Collector for remote ones) fills it before
    /// serving/streaming. Omitted-on-wire tolerant (`default` → `None`).
    #[serde(default)]
    pub diff: Option<DiffStat>,
    /// The machine this Session runs on — the host's name (C7). Stamped at the
    /// source: the board's own local runtime (and, later, a Collector on a remote
    /// machine) sets it to the local hostname before the Session leaves the watcher,
    /// so every card can show which machine an Agent Session is on. Like `diff`, the
    /// collector projection leaves it `None`; a `None` still serializes cleanly,
    /// keeping the field additive on the wire for local-only, pre-C7 boards.
    #[serde(default)]
    pub machine: Option<String>,
}
