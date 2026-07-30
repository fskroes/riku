//! Transcript parsing. Each transcript line is one JSON object; we tolerate
//! unknown fields, unknown `type` values, and schema drift between Claude Code
//! versions (unknown => ignore, never error). Only `user` / `assistant` entries
//! feed the session model.
//!
//! The same decoding serves both kinds of Claude transcript — an Agent Session's
//! and a Sub-agent's — because their entries have the same shape. Which of the two
//! a file is has already been decided from its path by the time a line gets here
//! (see `source.rs`); [`Entry::is_sidechain`] survives only so a parent's fold can
//! *ignore* a Sub-agent turn that a legacy transcript still carries inline, never to
//! fold one into the parent.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::fold::first_line;

/// A raw transcript line, deserialized leniently. Fields we do not model are
/// dropped; every field is optional so partial / drifting records still parse.
#[derive(Debug, Deserialize)]
struct RawEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    /// Claude Code marks a synthetic API-error turn with this flag (model
    /// `<synthetic>`); the newest such record raises Attention(Error).
    #[serde(rename = "isApiErrorMessage", default)]
    is_api_error_message: bool,
    message: Option<RawMessage>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMessage {
    model: Option<String>,
    usage: Option<Usage>,
    /// Why the assistant turn stopped. `tool_use` means it ended to call a tool
    /// (waiting on the human); `end_turn` / `stop_sequence` mean it finished.
    stop_reason: Option<String>,
    #[serde(default)]
    content: Content,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// An Anthropic message `content` is either a bare string or an array of blocks.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

impl Default for Content {
    fn default() -> Self {
        Content::Blocks(Vec::new())
    }
}

/// A content block. We distinguish text (for the activity line), `tool_use` (a
/// pending human need, correlated by its `id`), and `tool_result` (which answers a
/// `tool_use` by `tool_use_id`); every other block type collapses to `Other` rather
/// than failing. Extra fields on the blocks we do model are ignored.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum Block {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: Option<String>,
        name: Option<String>,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: Option<String> },
    #[serde(other)]
    Other,
}

/// A `tool_use` block the agent is waiting on: its correlation `id`, tool `name`,
/// and a short, source-faithful `detail` extracted from the call's input (a command,
/// path, or the like) for Attention Evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolUseInfo {
    pub id: Option<String>,
    pub name: Option<String>,
    pub detail: Option<String>,
    /// Who the call addresses, from the input's `to` field — the id of an existing
    /// Sub-agent, for the tool that sends a finished one back to work. `None` for
    /// every other call, which addresses nobody.
    pub recipient: Option<String>,
}

/// A `<task-notification>` record: the harness telling the orchestrator that
/// something it launched has ended, and how.
///
/// It is the **only** statement of a Sub-agent's completion. A spawn's `tool_result`
/// is a launch acknowledgement — "Async agent launched successfully… you will be
/// notified automatically when it completes" — arriving ~2s after the spawn against
/// children that run up to 20 minutes (ADR 0014).
///
/// Both fields are read from structured tags, never from the summary prose beside
/// them, and a record missing either is not a completion: notifications also carry
/// backgrounded commands and monitor events, which name no Sub-agent and state no
/// status. Joining by [`tool_use_id`](Self::tool_use_id) back to a spawn is what
/// keeps those out — 101 task-ids appear against 59 spawns, so counting
/// notifications would attribute a shell command to a Sub-agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskNotification {
    /// The tool-use this notification is about: the `Agent` spawn that started the
    /// Sub-agent, or the message that later resumed it.
    pub tool_use_id: String,
    /// The source's own id for the task — a Sub-agent's `agentId`. It is what a
    /// message resuming that Sub-agent addresses, so it is the identity that
    /// outlives any one tool-use id.
    pub task_id: Option<String>,
    /// How it ended, in the source's own word: `completed`, `failed`, `stopped`,
    /// `killed`. Nothing here interprets it, so a word we have not seen before travels
    /// exactly as the four we have do — up to the one bound every string the card
    /// carries shares (see [`tag`]), which no status word of this shape reaches.
    pub status: String,
}

/// What the accumulator cares about after decoding one relevant entry.
#[derive(Debug)]
pub struct Entry {
    pub is_assistant: bool,
    /// `isSidechain: true` — a Sub-agent's turn. In a Sub-agent's own transcript
    /// every entry carries it and it decides nothing (the path already classified
    /// the file). In a *parent's* transcript it should never appear: that traffic
    /// moved into the child files. Where a legacy transcript still carries one, the
    /// parent fold ignores it rather than folding a Sub-agent's turn as its own.
    pub is_sidechain: bool,
    pub session_id: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// First line of the first non-empty text block, truncated to 80 chars.
    pub activity: Option<String>,
    /// The `tool_use` blocks in this entry (assistant turns), in order. The last is
    /// the one an ended-to-call-a-tool turn is waiting on.
    pub tool_uses: Vec<ToolUseInfo>,
    /// The `tool_use_id`s answered by `tool_result` blocks in this entry (user
    /// turns) — the correlated resolutions of earlier tool-call needs.
    pub tool_result_ids: Vec<String>,
    /// The completion records this entry carries (user turns only). The harness
    /// writes each notification into the transcript three times — a `queue-operation`
    /// record, a queued-command `attachment`, and the user turn that actually reaches
    /// the orchestrator — and only the last is a `user` entry, so only the last is
    /// read. Latest-wins downstream makes the duplication harmless either way.
    pub task_notifications: Vec<TaskNotification>,
    /// `message.stop_reason`, when present (assistant turns only).
    pub stop_reason: Option<String>,
    /// `true` for a synthetic `isApiErrorMessage` record.
    pub is_api_error: bool,
}

/// Parse one transcript line.
///
/// * `Ok(Some(entry))` — a relevant `user` / `assistant` entry.
/// * `Ok(None)` — valid JSON we intentionally ignore (other `type`).
/// * `Err(_)` — the line is not valid JSON (malformed or a mid-write fragment).
pub fn parse_entry(line: &str) -> Result<Option<Entry>, serde_json::Error> {
    let raw: RawEntry = serde_json::from_str(line)?;

    let is_assistant = match raw.entry_type.as_deref() {
        Some("assistant") => true,
        Some("user") => false,
        // Unknown / irrelevant type (attachment, queue-operation, ...).
        _ => return Ok(None),
    };

    // A record with no message at all reads as an empty one: every field it would
    // have contributed is already an absence.
    let message = raw.message.unwrap_or_default();
    let usage = message.usage.unwrap_or_default();
    let summary = summarize_content(&message.content, is_assistant);

    Ok(Some(Entry {
        is_assistant,
        is_sidechain: raw.is_sidechain,
        session_id: raw.session_id,
        timestamp: raw.timestamp,
        cwd: raw.cwd,
        git_branch: raw.git_branch,
        model: message.model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        activity: summary.activity,
        tool_uses: summary.tool_uses,
        tool_result_ids: summary.tool_result_ids,
        task_notifications: summary.task_notifications,
        stop_reason: message.stop_reason,
        is_api_error: raw.is_api_error_message,
    }))
}

/// What one message's content contributes to the fold.
#[derive(Default)]
struct ContentSummary {
    activity: Option<String>,
    tool_uses: Vec<ToolUseInfo>,
    tool_result_ids: Vec<String>,
    task_notifications: Vec<TaskNotification>,
}

/// Extract the activity line (assistant text only), the pending `tool_use` calls, the
/// `tool_result` correlations, and — from a user turn's prompt text — the completion
/// records it carries.
fn summarize_content(content: &Content, is_assistant: bool) -> ContentSummary {
    let mut summary = ContentSummary::default();
    match content {
        // A notification reaches the orchestrator as a user turn whose whole prompt is
        // the record — a bare string in every one observed, but read out of text blocks
        // too, since the same prompt is a block list whenever anything accompanies it.
        Content::Text(s) => summary.add_text(s, is_assistant),
        Content::Blocks(blocks) => {
            for block in blocks {
                match block {
                    Block::Text { text } => summary.add_text(text, is_assistant),
                    Block::ToolUse { id, name, input } => summary.tool_uses.push(ToolUseInfo {
                        id: id.clone(),
                        name: name.clone(),
                        detail: tool_input_detail(input),
                        recipient: input
                            .as_object()
                            .and_then(|o| o.get("to"))
                            .and_then(|v| v.as_str())
                            .and_then(first_line),
                    }),
                    Block::ToolResult {
                        tool_use_id: Some(id),
                    } => summary.tool_result_ids.push(id.clone()),
                    _ => {}
                }
            }
        }
    }
    summary
}

impl ContentSummary {
    /// What one text block contributes, which turns on who wrote it: an assistant's
    /// text is the activity line — the first non-empty one wins — while a user's is a
    /// prompt, and a prompt is where completion records arrive.
    fn add_text(&mut self, text: &str, is_assistant: bool) {
        if is_assistant {
            if self.activity.is_none() {
                self.activity = first_line(text);
            }
        } else {
            self.task_notifications.extend(task_notifications(text));
        }
    }
}

/// The completion records in one user prompt, read from the tags the harness writes
/// rather than from the summary prose beside them.
///
/// A record naming no tool-use, or stating no status, is not a completion and is
/// dropped here: monitor events and backgrounded commands notify under the same tag,
/// and how a Sub-agent ended is read or it is absent.
fn task_notifications(text: &str) -> Vec<TaskNotification> {
    const OPEN: &str = "<task-notification>";
    const CLOSE: &str = "</task-notification>";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let body = &rest[start + OPEN.len()..];
        let (body, tail) = match body.find(CLOSE) {
            Some(end) => (&body[..end], &body[end + CLOSE.len()..]),
            // An unterminated record: read what is there rather than dropping it.
            None => (body, ""),
        };
        if let (Some(tool_use_id), Some(status)) = (tag(body, "tool-use-id"), tag(body, "status")) {
            out.push(TaskNotification {
                tool_use_id,
                task_id: tag(body, "task-id"),
                status,
            });
        }
        rest = tail;
    }
    out
}

/// The text of one `<name>…</name>` tag: its first line, bounded to 80 chars like
/// every other string the card carries. `None` when the tag is absent or says nothing.
///
/// The bound is safe for the tags read here, which are ids and a single word. Were an
/// id ever longer, truncating it would only cost the join a match — a notification
/// that names no spawn we hold changes nothing — never attach it to the wrong one.
fn tag(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{name}>"))?;
    first_line(&body[start..start + end])
}

/// A short, source-faithful detail from a `tool_use` call's input for evidence: the
/// first present of a handful of human-legible fields (a command, path, pattern, …),
/// or `None` when the input carries none. The whole argument object is never dumped
/// — only one recognized field — and sanitization/bounding still apply downstream.
fn tool_input_detail(input: &serde_json::Value) -> Option<String> {
    const FIELDS: [&str; 8] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
    ];
    let obj = input.as_object()?;
    FIELDS
        .iter()
        .find_map(|f| obj.get(*f).and_then(|v| v.as_str()))
        .and_then(first_line)
}
