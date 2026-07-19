//! Transcript parsing. Each transcript line is one JSON object; we tolerate
//! unknown fields, unknown `type` values, and schema drift between Claude Code
//! versions (unknown => ignore, never error). Only `user` / `assistant` entries
//! that are not sidechain traffic feed the session model.

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
    message: Option<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    model: Option<String>,
    usage: Option<Usage>,
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

/// A content block. We only distinguish text (for the activity line) from a
/// `tool_use` (for the Attention heuristic); every other block type — including
/// `tool_result` — collapses to `Other` rather than failing. Extra fields on the
/// blocks we do model are ignored.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum Block {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse,
    #[serde(other)]
    Other,
}

/// What the accumulator cares about after decoding one relevant entry.
#[derive(Debug)]
pub struct Entry {
    pub is_assistant: bool,
    pub session_id: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// First line of the first non-empty text block, truncated to 80 chars.
    pub activity: Option<String>,
    /// `true` if this entry contains at least one `tool_use` block.
    pub has_tool_use: bool,
}

/// Parse one transcript line.
///
/// * `Ok(Some(entry))` — a relevant, non-sidechain `user` / `assistant` entry.
/// * `Ok(None)` — valid JSON we intentionally ignore (sidechain, other `type`).
/// * `Err(_)` — the line is not valid JSON (malformed or a mid-write fragment).
pub fn parse_entry(line: &str) -> Result<Option<Entry>, serde_json::Error> {
    let raw: RawEntry = serde_json::from_str(line)?;

    // Subagent traffic never surfaces as its own card.
    if raw.is_sidechain {
        return Ok(None);
    }

    let is_assistant = match raw.entry_type.as_deref() {
        Some("assistant") => true,
        Some("user") => false,
        // Unknown / irrelevant type (attachment, queue-operation, ...).
        _ => return Ok(None),
    };

    let (model, input_tokens, output_tokens, activity, has_tool_use) = match raw.message {
        Some(msg) => {
            let (usage_in, usage_out) = msg
                .usage
                .map(|u| (u.input_tokens, u.output_tokens))
                .unwrap_or((0, 0));
            let (activity, has_tool_use) = summarize_content(&msg.content, is_assistant);
            (msg.model, usage_in, usage_out, activity, has_tool_use)
        }
        None => (None, 0, 0, None, false),
    };

    Ok(Some(Entry {
        is_assistant,
        session_id: raw.session_id,
        timestamp: raw.timestamp,
        cwd: raw.cwd,
        git_branch: raw.git_branch,
        model,
        input_tokens,
        output_tokens,
        activity,
        has_tool_use,
    }))
}

/// Extract the activity line (assistant text only) and whether a `tool_use` block
/// is present.
fn summarize_content(content: &Content, is_assistant: bool) -> (Option<String>, bool) {
    match content {
        Content::Text(s) => {
            let activity = if is_assistant { first_line(s) } else { None };
            (activity, false)
        }
        Content::Blocks(blocks) => {
            let mut activity = None;
            let mut has_tool_use = false;
            for block in blocks {
                match block {
                    Block::Text { text } if is_assistant && activity.is_none() => {
                        activity = first_line(text);
                    }
                    Block::ToolUse => has_tool_use = true,
                    _ => {}
                }
            }
            (activity, has_tool_use)
        }
    }
}
