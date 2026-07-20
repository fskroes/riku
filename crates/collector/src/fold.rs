//! The Session Source seam. Each tool (Claude Code, Codex CLI) decodes its own
//! transcript lines by implementing [`Fold`]; everything downstream — byte-offset
//! tailing, truncation reset, the mtime-based status heuristic, the [`Session`]
//! shape — is shared and lives in `session.rs`.
//!
//! A [`Fold`] folds a file's lines into a [`Projection`]: the tool-agnostic inputs
//! the shared builder needs. The status column is derived here from
//! [`Projection::attention`] plus file mtime, so no source reimplements it.

use chrono::{DateTime, Duration, Utc};

use crate::model::{AttentionReason, Status, Tool};

/// A session is Active/Attention while its file was touched within this window,
/// and Finished once it goes quiet. Locked for C1 (mtime only); shared by every
/// source.
pub const ACTIVITY_WINDOW: Duration = Duration::minutes(15);

/// The per-source projection of one transcript. Implementors own their line
/// format and their own token/model/activity accounting; the fields below are all
/// the shared builder reads back out.
pub trait Fold: Send {
    /// Fold one committed transcript line into the projection.
    ///
    /// Returns `false` only for a genuinely malformed line (valid line framing —
    /// a `\n`-terminated record — that does not parse), so the caller can `warn`
    /// and advance past it. Blank lines and intentionally-ignored records return
    /// `true`.
    fn apply_line(&mut self, line: &str) -> bool;

    /// Drop all accumulated state (the file was truncated or rewritten).
    fn reset(&mut self);

    /// The current projection, or `None` if no line has yet supplied a session id
    /// (or the file is one we suppress entirely, e.g. a Codex subagent rollout).
    fn projection(&self) -> Option<Projection>;
}

/// Tool-agnostic inputs the shared builder turns into a [`Session`]. Everything
/// except `status`, which the builder computes from `pending_input` and mtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    pub id: String,
    pub tool: Tool,
    pub project: String,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub activity: Option<String>,
    /// Latest entry timestamp; falls back to the file mtime when a source records
    /// no timestamps.
    pub last_event_at: Option<DateTime<Utc>>,
    /// Why the session needs a human, per the newest relevant signal each source
    /// decodes (Claude: an unanswered `tool_use` or an API error; Codex: an
    /// aborted turn or a pending approval), or `None` if nothing does. This is the
    /// only Attention input the shared rule reads.
    pub attention: Option<AttentionReason>,
}

/// The shared status rule. Attention outranks staleness: a present attention
/// reason wins regardless of the quiet window (so an old, still-unanswered wait
/// never ages into Finished); otherwise a quiet file is Finished; otherwise
/// Active. Staleness alone never produces Attention. Identical for every source.
pub fn status_for(
    attention: Option<AttentionReason>,
    mtime: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Status {
    if attention.is_some() {
        Status::Attention
    } else if now.signed_duration_since(mtime) >= ACTIVITY_WINDOW {
        Status::Finished
    } else {
        Status::Active
    }
}

/// First non-empty line of `s`, trimmed and truncated to 80 chars (by char).
/// Shared by every source's activity extraction.
pub(crate) fn first_line(s: &str) -> Option<String> {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(truncate_chars(line, 80))
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Last path segment of a `cwd` (the project name shown on a card). Shared.
pub(crate) fn project_from_cwd(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(cwd)
        .to_string()
}
