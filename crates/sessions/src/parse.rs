//! Transcript parsing. Each transcript line is one JSON object; we tolerate
//! unknown fields, unknown `type` values, and schema drift between Claude Code
//! versions (unknown => ignore, never error). Only `user` / `assistant` entries
//! feed the session model. Sidechain (Sub-agent) `user` / `assistant` entries are
//! parsed too, flagged with [`Entry::is_sidechain`]: the fold folds their token
//! usage and liveness into the parent rather than surfacing them as their own card.

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

#[derive(Debug, Deserialize)]
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
    ToolResult {
        tool_use_id: Option<String>,
    },
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
}

/// What the accumulator cares about after decoding one relevant entry.
#[derive(Debug)]
pub struct Entry {
    pub is_assistant: bool,
    /// `isSidechain: true` — Sub-agent traffic. The fold folds such an entry's token
    /// usage and recency into the parent instead of treating it as a card of its own.
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
    /// `message.stop_reason`, when present (assistant turns only).
    pub stop_reason: Option<String>,
    /// `true` for a synthetic `isApiErrorMessage` record.
    pub is_api_error: bool,
}

/// Parse one transcript line.
///
/// * `Ok(Some(entry))` — a relevant `user` / `assistant` entry (possibly sidechain,
///   flagged with [`Entry::is_sidechain`] for the fold to fold into the parent).
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

    let (model, input_tokens, output_tokens, activity, tool_uses, tool_result_ids, stop_reason) =
        match raw.message {
            Some(msg) => {
                let (usage_in, usage_out) = msg
                    .usage
                    .map(|u| (u.input_tokens, u.output_tokens))
                    .unwrap_or((0, 0));
                let summary = summarize_content(&msg.content, is_assistant);
                (
                    msg.model,
                    usage_in,
                    usage_out,
                    summary.activity,
                    summary.tool_uses,
                    summary.tool_result_ids,
                    msg.stop_reason,
                )
            }
            None => (None, 0, 0, None, Vec::new(), Vec::new(), None),
        };

    Ok(Some(Entry {
        is_assistant,
        is_sidechain: raw.is_sidechain,
        session_id: raw.session_id,
        timestamp: raw.timestamp,
        cwd: raw.cwd,
        git_branch: raw.git_branch,
        model,
        input_tokens,
        output_tokens,
        activity,
        tool_uses,
        tool_result_ids,
        stop_reason,
        is_api_error: raw.is_api_error_message,
    }))
}

/// What one message's content contributes to the fold.
#[derive(Default)]
struct ContentSummary {
    activity: Option<String>,
    tool_uses: Vec<ToolUseInfo>,
    tool_result_ids: Vec<String>,
}

/// Extract the activity line (assistant text only), the pending `tool_use` calls,
/// and the `tool_result` correlations from a message's content.
fn summarize_content(content: &Content, is_assistant: bool) -> ContentSummary {
    match content {
        Content::Text(s) => ContentSummary {
            activity: is_assistant.then(|| first_line(s)).flatten(),
            ..Default::default()
        },
        Content::Blocks(blocks) => {
            let mut summary = ContentSummary::default();
            for block in blocks {
                match block {
                    Block::Text { text } if is_assistant && summary.activity.is_none() => {
                        summary.activity = first_line(text);
                    }
                    Block::ToolUse { id, name, input } => summary.tool_uses.push(ToolUseInfo {
                        id: id.clone(),
                        name: name.clone(),
                        detail: tool_input_detail(input),
                    }),
                    Block::ToolResult {
                        tool_use_id: Some(id),
                    } => summary.tool_result_ids.push(id.clone()),
                    _ => {}
                }
            }
            summary
        }
    }
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
