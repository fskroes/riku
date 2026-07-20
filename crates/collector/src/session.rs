//! Two things live here: the Claude Code [`Fold`] ([`Accumulator`]) and the shared
//! incremental tail ([`FileState`]) that drives any source's fold.
//!
//! [`Accumulator`] decodes Claude Code transcript entries and folds them into a
//! [`Projection`]. [`FileState`] is source-agnostic: it owns the byte-offset
//! bookkeeping, truncation reset, malformed-line handling, and the final
//! [`Session`] assembly (status via [`status_for`]) for whatever [`Fold`] it holds.

use chrono::{DateTime, Utc};
use tracing::warn;

use crate::fold::{project_from_cwd, status_for, Fold, Projection};
use crate::model::{AttentionReason, Session, Status, Tool};
use crate::parse::{parse_entry, Entry};

/// The Attention-relevant classification of the newest relevant Claude entry seen
/// so far. Because entries fold in file order, the last one to set this wins, so
/// a `tool_result` or a fresh turn after an error/approval naturally clears it
/// (recovery needs no extra bookkeeping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastKind {
    /// No relevant entry yet.
    None,
    /// Newest entry is an assistant turn that ended to call a tool
    /// (`stop_reason: tool_use`, or a `tool_use` block when `stop_reason` is
    /// absent) — i.e. Claude is waiting on the human, and nothing answers it later.
    AwaitingTool,
    /// Newest relevant entry is an API-error record (`isApiErrorMessage: true`).
    ApiError,
    /// Newest entry is anything else (a user turn, an assistant turn that ended
    /// cleanly, ...) — the session needs nothing from the human.
    Other,
}

impl LastKind {
    fn attention(self) -> Option<AttentionReason> {
        match self {
            LastKind::AwaitingTool => Some(AttentionReason::Waiting),
            LastKind::ApiError => Some(AttentionReason::Error),
            LastKind::None | LastKind::Other => None,
        }
    }
}

/// The Claude Code fold: a running projection of a single transcript. Fold entries
/// in file order via [`Accumulator::apply`]; token counts and "latest" fields
/// update in place.
#[derive(Debug)]
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
    last_kind: LastKind,
}

impl Default for Accumulator {
    fn default() -> Self {
        Accumulator {
            id: None,
            project: None,
            cwd: None,
            branch: None,
            latest_timestamp: None,
            model: None,
            activity: None,
            tokens_in: 0,
            tokens_out: 0,
            last_kind: LastKind::None,
        }
    }
}

impl Accumulator {
    /// Fold one already-parsed relevant entry into the projection.
    pub fn apply(&mut self, entry: Entry) {
        self.tokens_in += entry.input_tokens;
        self.tokens_out += entry.output_tokens;

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
        if let Some(ts) = entry.timestamp {
            // Entries arrive in file (chronological) order, but guard anyway.
            if self.latest_timestamp.is_none_or(|cur| ts >= cur) {
                self.latest_timestamp = Some(ts);
            }
        }

        if entry.is_api_error {
            // A synthetic API-error record (model `<synthetic>`, no real content):
            // record the error but never let it overwrite the card's model/activity.
            self.last_kind = LastKind::ApiError;
        } else if entry.is_assistant {
            if entry.model.is_some() {
                self.model = entry.model;
            }
            if entry.activity.is_some() {
                self.activity = entry.activity;
            }
            // Waiting-on-human = the turn ended to call a tool. Prefer the explicit
            // `stop_reason`; fall back to the presence of a `tool_use` block only
            // when `stop_reason` is absent. This stops a mid-run tool_use or a
            // cleanly-ended (`end_turn`) turn from masquerading as a wait.
            let awaiting = match entry.stop_reason.as_deref() {
                Some("tool_use") => true,
                Some(_) => false,
                None => entry.has_tool_use,
            };
            self.last_kind = if awaiting {
                LastKind::AwaitingTool
            } else {
                LastKind::Other
            };
        } else {
            // A user turn (e.g. a tool_result answering the previous tool_use, or
            // any activity resuming after an error) clears the attention state.
            self.last_kind = LastKind::Other;
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

    fn projection(&self) -> Option<Projection> {
        let id = self.id.clone()?;
        Some(Projection {
            id,
            tool: Tool::Claude,
            project: self.project.clone().unwrap_or_default(),
            model: self.model.clone(),
            branch: self.branch.clone(),
            cwd: self.cwd.clone(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            activity: self.activity.clone(),
            last_event_at: self.latest_timestamp,
            attention: self.last_kind.attention(),
        })
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

    /// Build the UI [`Session`], given the file's mtime (which drives status).
    /// Returns `None` if the fold has no projection yet.
    pub fn build(&self, mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<Session> {
        let p = self.fold.projection()?;
        let status = status_for(p.attention, mtime, now);
        // Carry the reason only when the status is actually Attention, so the two
        // can never disagree on the wire.
        let attention_reason = (status == Status::Attention)
            .then_some(p.attention)
            .flatten();
        // Cost is pure (tokens × the model's list price); computed before `p.model`
        // is moved into the session below. The live git `diff` is out-of-transcript,
        // so the board fills it and the collector leaves it None.
        let cost_usd =
            crate::pricing::estimate_cost_usd(p.model.as_deref(), p.tokens_in, p.tokens_out);
        Some(Session {
            id: p.id,
            tool: p.tool,
            project: p.project,
            model: p.model,
            branch: p.branch,
            cwd: p.cwd,
            tokens_in: p.tokens_in,
            tokens_out: p.tokens_out,
            activity: p.activity,
            last_event_at: p.last_event_at.unwrap_or(mtime),
            status,
            attention_reason,
            cost_usd,
            diff: None,
            // Stamped by the board's runtime (or a Collector) at the source, not by
            // the transcript projection — see `Session::machine`.
            machine: None,
        })
    }
}

/// Minimal byte search; avoids pulling in the `memchr` crate for one call site.
fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    fn claude() -> FileState {
        FileState::new(Box::new(Accumulator::default()))
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

    #[test]
    fn sidechain_entries_are_ignored() {
        let mut fs = claude();
        let line = r#"{"type":"assistant","isSidechain":true,"sessionId":"sub","cwd":"/a/b","message":{"model":"m","usage":{"input_tokens":999,"output_tokens":999},"content":[{"type":"text","text":"noise"}]}}"#;
        let mut data = line.to_string();
        data.push('\n');
        data.push_str(&assistant("main", "real", 5, 1));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(s.id, "main");
        assert_eq!(s.tokens_in, 5); // sidechain 999 not counted
        assert_eq!(s.tokens_out, 1);
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
        assert_eq!(s.attention_reason, Some(AttentionReason::Waiting));
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
        assert_eq!(s.attention_reason, None);
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
        assert_eq!(s.attention_reason, Some(AttentionReason::Error));
        // The synthetic record must not clobber the card's real model/activity.
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s.activity.as_deref(), Some("hello"));
    }

    #[test]
    fn old_unanswered_wait_stays_attention_not_finished() {
        // Precedence: a present attention reason outranks staleness (story 6).
        let mut fs = claude();
        let mut data = assistant_tool_use("s1");
        data.push('\n');
        fs.feed(data.as_bytes());

        // File quiet for 30 min (well past ACTIVITY_WINDOW), yet still waiting.
        let s = fs
            .build(ts("2026-07-19T10:00:00Z"), ts("2026-07-19T10:30:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Attention);
        assert_eq!(s.attention_reason, Some(AttentionReason::Waiting));
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
        assert_eq!(s.attention_reason, None);
    }

    #[test]
    fn quiet_session_is_finished() {
        let mut fs = claude();
        let mut data = assistant("s1", "done", 1, 1);
        data.push('\n');
        fs.feed(data.as_bytes());

        // mtime 20 min before now.
        let now = ts("2026-07-19T10:20:00Z");
        let s = fs.build(ts("2026-07-19T10:00:00Z"), now).unwrap();
        assert_eq!(s.status, Status::Finished);
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
