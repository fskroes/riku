//! The Session model: what the board renders as a card, serialized to the UI as
//! camelCase JSON.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Board status for an Agent Session. See CONTEXT.md and issue #2 for the locked
/// C1 heuristic (mtime-based; process liveness lands with C3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Touched within the activity window and not waiting on a human.
    Active,
    /// The newest entry is an assistant turn holding an unanswered `tool_use`
    /// (a pending permission / question) and the file is fresh.
    Attention,
    /// Untouched for at least the activity window.
    Finished,
}

/// One Agent Session — a single Claude Code transcript, projected for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// `sessionId` (uuid). Globally unique, so duplicate filename stems across
    /// project dirs cannot collide.
    pub id: String,
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
}
