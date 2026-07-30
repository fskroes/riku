//! Transcript parsing. Each transcript line is one JSON object; we tolerate
//! unknown fields, unknown `type` values, and schema drift between Claude Code
//! versions (unknown => ignore, never error). `user` / `assistant` entries feed the
//! session model; every other record type is ignored except for the one thing such a
//! record can be the only carrier of — a queued completion notification (see
//! [`Entry::task_notifications`]).
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
    /// A `queue-operation` record's whole payload: the prompt that was queued, a bare
    /// string in every record observed. Read only for records that are not turns, where
    /// it can be the sole statement of a Sub-agent's ending.
    ///
    /// Typed as a `Value` rather than a `String` because this field name is shared with
    /// record types we do not model, where it may hold anything: a `String` here would
    /// make one of those a *deserialization error*, and an `Err` from
    /// [`parse_entry`] means "mid-write fragment, retry this line" — a whole transcript
    /// would stall on a record it should have ignored. A payload of another shape is an
    /// absence.
    #[serde(default)]
    content: Option<serde_json::Value>,
    /// A queued-command `attachment`, whose `prompt` carries the same payload.
    #[serde(default)]
    attachment: Option<RawAttachment>,
    /// What a `queue-operation` record is doing to the prompt it carries. Read only to
    /// recognise the one operation that restates a payload rather than delivering it
    /// (see [`DEQUEUE_OPERATION`]). Lenient for the same reason [`RawEntry::content`]
    /// is: an unmodelled record using this field name must be an absence, not a stall.
    #[serde(default)]
    operation: Option<serde_json::Value>,
}

/// The part of an `attachment` record worth reading. Deliberately shapeless beyond
/// `prompt`: attachments carry many kinds of thing and requiring the notification tags
/// is what selects the one kind meant here. `prompt` is lenient for the same reason
/// [`RawEntry::content`] is.
#[derive(Debug, Deserialize)]
struct RawAttachment {
    #[serde(default)]
    prompt: Option<serde_json::Value>,
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
///
/// `Default` is every field's absence — no tokens, no activity, nothing pending. It
/// exists so a record that contributes exactly one thing can say only that thing
/// (see [`queued_notification`]) without restating the other fourteen, which would
/// make adding a field a two-site edit and a silent one to get wrong.
#[derive(Debug, Default)]
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
    /// The completion records this entry carries.
    ///
    /// A notification reaches the orchestrator as a user turn whose whole prompt is the
    /// record — but **only when the parent was idle** when its child ended. A parent
    /// that is mid-turn, which is what an orchestrator that just fanned out is, has the
    /// notification *enqueued* instead: the only records written are a `queue-operation`
    /// (`enqueue`, then `remove`) and a queued-command `attachment`, and no user turn is
    /// ever written. Reading only the user turn dropped 33 of 92 spawns on the corpus
    /// and left 20 of 41 fan-out parents pinned Active by a row stuck Running
    /// (issue #85), so a completion is read out of whichever record carries it.
    ///
    /// Reading all of them is harmless: the join is by tool-use id and the latest word
    /// wins, so repeats of one notification land on one row and restate one outcome.
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
        // Not a turn. Such a record contributes nothing the card is built from — no
        // tokens, no activity, no Attention — with one exception: it may be the only
        // record a Sub-agent's ending was ever written into.
        _ => return Ok(queued_notification(raw)),
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

/// The `queue-operation` that takes a queued prompt back off the queue, restating the
/// payload it was parked with. [`queued_notification`] refuses it.
///
/// **It is the only carrier that lands late.** Timed across this machine's corpus, the
/// `attachment` form lags the `enqueue` by a median *and* maximum of 0.0s and the user
/// turn by 0.0s median, while this one lags it by a median of 9.4s and up to 903.8s — 53
/// of its 60 appearances by more than a second, which is room enough for a resume to land
/// in between and be overwritten by news already superseded (issue #87).
///
/// **Refusing it drops nothing.** All 60 of those appearances echo an `enqueue` already
/// on disk, and no notification anywhere on the corpus is carried by a `remove` alone.
///
/// **Named as the record to refuse, not as the records to accept.** Reading "only an
/// `enqueue`" would be the same one line and look tidier, but the two orientations fail
/// in opposite directions under the format drift this module is deliberately built for.
/// A harness that renames this operation reintroduces issue #87 — one roster row briefly
/// wrong in the direction that self-corrects on the child's next notification. A harness
/// that renamed `enqueue` under an allow-list would drop **every** queued notification
/// and hand back issue #85 in full: a third of all spawns stuck Running for the life of
/// the process, and their parents' cards pinned with them. So anything unrecognised is
/// read, and only this is not. Same class of hazard as `Task` becoming `Agent`
/// (ADR 0014) and as issue #78 — chosen so drift costs the cheaper mistake.
const DEQUEUE_OPERATION: &str = "remove";

/// A completion notification carried by a record that is not a turn: the
/// `queue-operation` that enqueued it, or the queued-command `attachment` holding the
/// same prompt. `None` unless such a record carries the notification tags at all.
///
/// The record *type* is deliberately not part of the test — the notification tags are.
/// A harness that renames `queue-operation` keeps working, which matters for a format
/// nobody here controls: the rename of `Task` to `Agent` is what hid this whole feature
/// for months (ADR 0014), and the same class of drift is issue #78.
///
/// Everything a turn would contribute is left at its absence, so such a record states
/// the ending, says which session's file it is (identity is assigned unconditionally,
/// ADR 0014's second breakage), and advances the session's latest timestamp — the
/// notification *is* an event in that session, and the user-turn form advanced it too.
/// It contributes no tokens, no activity, no model, and no Attention of any kind: it is
/// not a user turn, so it neither raises a human need nor answers one (ADR 0010).
///
/// A [`DEQUEUE_OPERATION`] record is the one that states no ending — but it is still
/// such a record, and everything above still holds of it.
fn queued_notification(raw: RawEntry) -> Option<Entry> {
    // A record that says it is *removing* a queued prompt is not delivering that
    // prompt's news; it is the queue restating a payload it has already handed over. It
    // is the only carrier written at a moment other than the notification's arrival, so
    // it is the only one that can restate a status *across* a later event — and the
    // event it would overwrite is a resume. See [`DEQUEUE_OPERATION`] and ADR 0014's
    // #87 update for the measurements.
    //
    // What is refused is the *news*, not the record. Returning `None` here instead would
    // take the dequeue's timestamp and its statement of session identity down with the
    // outcome — and an early return positioned above that assignment is ADR 0014's second
    // recorded breakage, which is why `Accumulator::apply` makes both unconditional. The
    // queue draining is a real event in this transcript at the moment it is written; only
    // the outcome it repeats is stale.
    //
    // Matched on the operation alone, without requiring the record type, for the same
    // reason the rest of this function ignores the type: nothing on the corpus carries a
    // notification under a top-level `remove` operation except the queue.
    let is_dequeue_echo =
        raw.operation.as_ref().and_then(|o| o.as_str()) == Some(DEQUEUE_OPERATION);
    let carrier = raw
        .content
        .or_else(|| raw.attachment.and_then(|a| a.prompt))?;
    let notifications = task_notifications(carrier.as_str()?);
    if notifications.is_empty() {
        return None;
    }
    let notifications = if is_dequeue_echo {
        Vec::new()
    } else {
        notifications
    };
    Some(Entry {
        is_sidechain: raw.is_sidechain,
        session_id: raw.session_id,
        timestamp: raw.timestamp,
        cwd: raw.cwd,
        git_branch: raw.git_branch,
        task_notifications: notifications,
        ..Entry::default()
    })
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
