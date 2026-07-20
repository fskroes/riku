//! The Session model: what the board renders as a card, serialized to the UI as
//! camelCase JSON.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// The agent tool a Session came from. One Session Source per tool; carried to
/// the UI so each card can show the right tool tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Claude,
    Codex,
}

/// Board status for an Agent Session. The flat three-value shape keeps the client
/// simple; `Attention` carries its cause in [`Session::attention_reason`]. See
/// CONTEXT.md, issue #2 (mtime-based Active↔Finished) and issue #7 (principled,
/// source-agnostic Attention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionReason {
    /// Waiting on a human — a pending approval or question.
    Waiting,
    /// The session ended in an error (an abnormal / aborted ending).
    Error,
}

/// One Agent Session — a single Claude Code transcript, projected for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}
