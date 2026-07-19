//! The Codex CLI [`Fold`]. Decodes `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`
//! rollouts into a [`Projection`].
//!
//! Each line is a JSON object with a top-level `timestamp`, a `type`, and a
//! `payload`. We model just enough of four `type`s and ignore everything else —
//! Codex's schema drifts between CLI versions, so unknown `type`s, unknown
//! `payload.type`s, and unknown fields are dropped, never errored:
//!
//! * `session_meta` (first line) — `payload.id` is the Session id; `thread_source
//!   == "subagent"` marks a subagent rollout we suppress entirely (Codex's analog
//!   of Claude's `isSidechain`); `payload.cwd` and `payload.git.branch` seed the
//!   project/branch.
//! * `turn_context` — the model lives here (`payload.model`), per turn, not on the
//!   message; latest wins.
//! * `event_msg` / `token_count` — `payload.info.total_token_usage` is *cumulative
//!   for the session*, so we take the latest one rather than summing.
//! * `response_item` / `message` with `role == "assistant"` — its `output_text`
//!   drives the activity line.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::fold::{first_line, project_from_cwd, Fold, Projection};
use crate::model::Tool;

/// A raw Codex rollout line, deserialized leniently. Every field is optional so
/// partial / drifting records still parse; unmodeled fields are dropped.
#[derive(Debug, Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    line_type: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    payload: Option<Payload>,
}

/// A flat union of every `payload` shape we read. Field sets are disjoint across
/// the `type`s we care about, so one lenient struct covers all of them.
#[derive(Debug, Default, Deserialize)]
struct Payload {
    // session_meta
    id: Option<String>,
    thread_source: Option<String>,
    git: Option<Git>,
    // session_meta + turn_context
    cwd: Option<String>,
    // turn_context
    model: Option<String>,
    // event_msg / response_item discriminator
    #[serde(rename = "type")]
    payload_type: Option<String>,
    // event_msg: token_count
    info: Option<TokenInfo>,
    // response_item: message
    role: Option<String>,
    content: Option<Vec<ContentBlock>>,
}

#[derive(Debug, Deserialize)]
struct Git {
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenInfo {
    /// Cumulative usage for the whole session (not per-turn).
    total_token_usage: Option<TokenUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// A `message` content block. We only need the text of `output_text` /
/// `input_text` blocks; every other block type collapses to an empty `text`.
#[derive(Debug, Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

/// The Codex fold: a running projection of a single rollout.
#[derive(Debug, Default)]
pub struct CodexFold {
    id: Option<String>,
    /// `thread_source == "subagent"` — the whole file is suppressed.
    is_subagent: bool,
    project: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    /// Latest `turn_context.model`.
    model: Option<String>,
    activity: Option<String>,
    /// Latest cumulative `total_token_usage`, not a running sum.
    tokens_in: u64,
    tokens_out: u64,
    latest_timestamp: Option<DateTime<Utc>>,
}

impl CodexFold {
    fn apply(&mut self, raw: RawLine) {
        if let Some(ts) = raw.timestamp {
            if self.latest_timestamp.is_none_or(|cur| ts >= cur) {
                self.latest_timestamp = Some(ts);
            }
        }

        let payload = match raw.payload {
            Some(p) => p,
            None => return,
        };

        match raw.line_type.as_deref() {
            Some("session_meta") => {
                if payload.id.is_some() {
                    self.id = payload.id;
                }
                if payload.thread_source.as_deref() == Some("subagent") {
                    self.is_subagent = true;
                }
                if let Some(cwd) = payload.cwd {
                    self.project = Some(project_from_cwd(&cwd));
                    self.cwd = Some(cwd);
                }
                if let Some(branch) = payload.git.and_then(|g| g.branch) {
                    self.branch = Some(branch);
                }
            }
            Some("turn_context") => {
                // Model comes from the turn, not the message; latest wins.
                if payload.model.is_some() {
                    self.model = payload.model;
                }
                if let Some(cwd) = payload.cwd {
                    self.project = Some(project_from_cwd(&cwd));
                    self.cwd = Some(cwd);
                }
            }
            Some("event_msg") => {
                if payload.payload_type.as_deref() == Some("token_count") {
                    // Cumulative for the session — overwrite, do not sum.
                    if let Some(usage) = payload.info.and_then(|i| i.total_token_usage) {
                        self.tokens_in = usage.input_tokens;
                        self.tokens_out = usage.output_tokens;
                    }
                }
            }
            Some("response_item")
                if payload.payload_type.as_deref() == Some("message")
                    && payload.role.as_deref() == Some("assistant") =>
            {
                if let Some(activity) = activity_from_content(&payload.content) {
                    self.activity = Some(activity);
                }
            }
            _ => {}
        }
    }
}

impl Fold for CodexFold {
    fn apply_line(&mut self, line: &str) -> bool {
        if line.trim().is_empty() {
            return true;
        }
        match serde_json::from_str::<RawLine>(line) {
            Ok(raw) => {
                self.apply(raw);
                true
            }
            Err(_) => false,
        }
    }

    fn reset(&mut self) {
        *self = CodexFold::default();
    }

    fn projection(&self) -> Option<Projection> {
        // Subagent rollouts never surface as their own card.
        if self.is_subagent {
            return None;
        }
        let id = self.id.clone()?;
        Some(Projection {
            id,
            tool: Tool::Codex,
            project: self.project.clone().unwrap_or_default(),
            model: self.model.clone(),
            branch: self.branch.clone(),
            cwd: self.cwd.clone(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            activity: self.activity.clone(),
            last_event_at: self.latest_timestamp,
            // Codex's approval-wait signal is unverified (all sampled rollouts ran
            // `approval_policy: never`); Attention for Codex is deferred to C3.
            pending_input: false,
        })
    }
}

/// First non-empty text line across a message's content blocks, truncated to 80.
fn activity_from_content(content: &Option<Vec<ContentBlock>>) -> Option<String> {
    let blocks = content.as_ref()?;
    blocks
        .iter()
        .filter_map(|b| b.text.as_deref())
        .find_map(first_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::FileState;

    fn codex() -> FileState {
        FileState::new(Box::new(CodexFold::default()))
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn meta(id: &str, thread_source: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "cwd": "/Users/x/repos/foo",
                "thread_source": thread_source,
                "git": { "branch": "main" }
            }
        })
        .to_string()
    }

    fn turn_context(model: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:01Z",
            "type": "turn_context",
            "payload": { "model": model, "cwd": "/Users/x/repos/foo", "approval_policy": "never" }
        })
        .to_string()
    }

    fn token_count(input: u64, output: u64) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": { "input_tokens": input, "output_tokens": output }
                }
            }
        })
        .to_string()
    }

    fn assistant_message(text: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }]
            }
        })
        .to_string()
    }

    fn feed(fs: &mut FileState, lines: &[String]) {
        let mut body = lines.join("\n");
        body.push('\n');
        fs.feed(body.as_bytes());
    }

    #[test]
    fn builds_a_codex_session() {
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                turn_context("gpt-5.6-sol"),
                token_count(1000, 200),
                assistant_message("wiring the collector"),
            ],
        );
        let s = fs.build(ts("2026-07-19T10:01:00Z"), ts("2026-07-19T10:02:00Z")).unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tool, Tool::Codex);
        assert_eq!(s.project, "foo");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(s.tokens_in, 1000);
        assert_eq!(s.tokens_out, 200);
        assert_eq!(s.activity.as_deref(), Some("wiring the collector"));
    }

    #[test]
    fn subagent_rollout_yields_no_card() {
        let mut fs = codex();
        feed(&mut fs, &[meta("sub-1", "subagent"), token_count(999, 999)]);
        assert!(fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).is_none());
    }

    #[test]
    fn token_count_is_cumulative_not_summed() {
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                token_count(1000, 200),
                token_count(1700, 350),
            ],
        );
        let s = fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).unwrap();
        // Latest cumulative total, not 1000+1700.
        assert_eq!(s.tokens_in, 1700);
        assert_eq!(s.tokens_out, 350);
    }

    #[test]
    fn model_comes_from_latest_turn_context() {
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                turn_context("gpt-5.6-sol"),
                turn_context("gpt-5.6-terra"),
            ],
        );
        let s = fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).unwrap();
        assert_eq!(s.model.as_deref(), Some("gpt-5.6-terra"));
    }

    #[test]
    fn missing_thread_source_still_renders() {
        // Older rollouts omit thread_source entirely — treat as a user session.
        let mut fs = codex();
        let meta = serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": { "id": "rollout-old", "cwd": "/a/foo" }
        })
        .to_string();
        feed(&mut fs, &[meta, token_count(10, 2)]);
        let s = fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).unwrap();
        assert_eq!(s.id, "rollout-old");
    }

    #[test]
    fn null_token_info_is_tolerated() {
        // Some token_count events carry `info: null`.
        let mut fs = codex();
        let null_tc = serde_json::json!({
            "timestamp": "2026-07-19T10:00:02Z",
            "type": "event_msg",
            "payload": { "type": "token_count", "info": null }
        })
        .to_string();
        feed(&mut fs, &[meta("rollout-1", "user"), token_count(50, 5), null_tc]);
        let s = fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).unwrap();
        assert_eq!(s.tokens_in, 50); // last real count retained
    }

    #[test]
    fn unknown_types_and_fields_are_ignored() {
        let mut fs = codex();
        let weird = r#"{"timestamp":"2026-07-19T10:00:05Z","type":"future_event","payload":{"whatever":1},"extra":true}"#.to_string();
        feed(&mut fs, &[meta("rollout-1", "user"), weird, token_count(7, 1)]);
        let s = fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tokens_in, 7);
    }

    #[test]
    fn no_session_meta_yields_no_card() {
        let mut fs = codex();
        // token_count before any session_meta: no id, so no card.
        feed(&mut fs, &[token_count(10, 2)]);
        assert!(fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).is_none());
    }

    #[test]
    fn malformed_line_is_skipped() {
        let mut fs = codex();
        let mut body = String::from("{ not valid json\n");
        body.push_str(&meta("rollout-1", "user"));
        body.push('\n');
        body.push_str(&token_count(9, 3));
        body.push('\n');
        fs.feed(body.as_bytes());
        let s = fs.build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z")).unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tokens_in, 9);
    }
}
