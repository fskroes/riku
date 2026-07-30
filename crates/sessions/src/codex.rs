//! The Codex CLI [`Fold`]. Decodes `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`
//! rollouts into a [`Projection`].
//!
//! Each line is a JSON object with a top-level `timestamp`, a `type`, and a
//! `payload`. We model just enough of four `type`s and ignore everything else —
//! Codex's schema drifts between CLI versions, so unknown `type`s, unknown
//! `payload.type`s, and unknown fields are dropped, never errored:
//!
//! * `session_meta` (first line) — `payload.id` is the Session id; `thread_source
//!   == "subagent"` marks a subagent rollout, which folds like any other but is
//!   stated as a Sub-agent rather than an Agent Session, so it never becomes a
//!   card; `payload.cwd` and `payload.git.branch` seed the project/branch. On a
//!   subagent rollout `payload.parent_thread_id` names the thread that spawned it —
//!   which may be another Sub-agent — and `payload.source.subagent.thread_spawn`
//!   carries the spawn depth.
//! * `turn_context` — the model lives here (`payload.model`), per turn, not on the
//!   message; latest wins.
//! * `event_msg` / `token_count` — `payload.info.total_token_usage` is *cumulative
//!   for the session*, so we take the latest one rather than summing.
//! * `response_item` / `message` with `role == "assistant"` — its `output_text`
//!   drives the activity line.
//!
//! Attention (C3, issue #7) is derived from the newest lifecycle event:
//! `turn_aborted` (an interrupted / killed turn) raises Attention(Error); a
//! `task_started` / `task_complete` / assistant message clears it (recovery is
//! free). Codex approval-waits are handled by [`is_approval_request`] — see its
//! note; no local rollout could verify the marker (all ran `approval_policy:
//! never`), so a real approval degrades safely to no-attention if the CLI names
//! the event differently.
//!
//! On a **subagent** rollout those same lifecycle events say something else as well:
//! `task_complete` is the terminal event, the one record that says the Sub-agent is
//! done, and Codex names exactly one word for it. A later `task_started` is a
//! resumption and takes the word back. None of it reaches Attention — a Sub-agent
//! projection carries none, so a Sub-agent that fails is reported to the agent that
//! sent it and never to the person (ADR 0010, ADR 0014).

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::attention::{AttentionReducer, NeedEvidence, Observation};
use crate::fold::{
    first_line, project_from_cwd, Attachment, Fold, Folded, Projection, SubAgentProjection,
};
use crate::model::{AttentionCause, Tool};

/// The outcome word a Codex Sub-agent carries once it reaches its terminal event.
///
/// **Riku's word for Codex's one event, not a token lifted out of the rollout.** The
/// record states a type, `task_complete`, and no `<status>`-style field anywhere —
/// unlike Claude, whose notification names one of `completed` / `failed` / `stopped` /
/// `killed` and where the outcome genuinely is the source's own word. So the mapping
/// is stated here, in one place, rather than being read: one terminal event, one word.
///
/// What that buys, and what it costs, is the same thing — Codex names no vocabulary
/// for any other ending, so there is nothing to map it onto. A Codex Sub-agent that
/// stops another way therefore carries no word at all rather than an invented one,
/// which is the Errand rule (CONTEXT.md) applied to outcomes: present when the source
/// says so, absent otherwise. A second Codex terminal event would need its own word
/// added beside this one; it must never be inferred from the prose next to it.
const COMPLETED: &str = "completed";

/// Where this rollout's latest lifecycle event leaves the Sub-agent it may be.
///
/// One value rather than a state flag beside an outcome string, so "finished, and
/// Codex has no word for how" stays sayable and "running, but here is how it ended"
/// stays unsayable. The default is [`Running`](Self::Running) — a rollout that has
/// stated no lifecycle event yet has not ended — which is the opposite of
/// [`SubAgentState`]'s own wire default, where an unreadable row must not claim to be
/// running.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum Lifecycle {
    /// No terminal event, or a later `task_started` took one back: a Sub-agent can
    /// resume after finishing, so a terminal event is the latest word, not a final one.
    #[default]
    Running,
    /// A terminal event, carrying the word for it when Codex has one — which is only
    /// for `task_complete`. An aborted turn ends the Sub-agent just as truly and ends
    /// it unworded, rather than leaving a row that claims to still be running and
    /// holds its parent out of Finished for as long as the parent lives.
    Ended(Option<String>),
}

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
    /// session_meta, subagent rollouts: the thread that spawned this one, which may
    /// itself be a Sub-agent. Read from the top level, where all 79 observed subagent
    /// rollouts state it; the copy inside the spawn block is on only 75 of them.
    parent_thread_id: Option<String>,
    /// session_meta, subagent rollouts: the spawn block, holding the depth. Untyped
    /// on purpose — see [`spawn_depth`].
    source: Option<serde_json::Value>,
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
    // approval request: the call's correlation id and (local-only) command.
    call_id: Option<String>,
    command: Option<serde_json::Value>,
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
    /// `thread_source == "subagent"` — the whole file is a Sub-agent's, so the fold
    /// states a Sub-agent projection instead of an Agent Session one.
    is_subagent: bool,
    /// The thread that spawned this one, when this rollout is a Sub-agent's. Codex
    /// names the *immediate* spawner, so the walk to the root happens in the store.
    parent_thread_id: Option<String>,
    /// How deep this Sub-agent was spawned; `0` when the rollout states no depth.
    depth: u32,
    /// Where this rollout's lifecycle events leave the Sub-agent it may be. Read only
    /// on that branch: an Agent Session's own liveness is Process Liveness and
    /// Staleness (ADR 0011), never its transcript's say-so.
    lifecycle: Lifecycle,
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
    /// The shared Attention lifecycle. This fold only translates Codex lifecycle
    /// records into normalized observations: `turn_aborted` → a Session error need,
    /// an `*_approval_request` → an Approval need, and `task_started` /
    /// `task_complete` / an assistant message → forward progress (Superseded).
    attention: AttentionReducer,
}

impl CodexFold {
    fn apply(&mut self, raw: RawLine) {
        if let Some(ts) = raw.timestamp {
            if self.latest_timestamp.is_none_or(|cur| ts >= cur) {
                self.latest_timestamp = Some(ts);
            }
        }

        // Attention Since prefers the record's own timestamp, falling back to the
        // latest one seen so far (set just above) when a record carries none.
        let at = raw.timestamp.or(self.latest_timestamp);

        let payload = match raw.payload {
            Some(p) => p,
            None => return,
        };

        // A pending approval can surface either as its own line `type` or as an
        // `event_msg` `payload.type`; accept both. Unverified against a live run
        // (see `approval_kind`), so an unrecognised name simply never fires. The
        // command is local-only evidence; only the structured kind label crosses
        // the wire (ADR 0010 allowlist).
        if let Some(kind) = approval_kind(raw.line_type.as_deref())
            .or_else(|| approval_kind(payload.payload_type.as_deref()))
        {
            self.attention.apply(Observation::Need {
                key: payload.call_id.clone().unwrap_or_else(|| "approval".into()),
                cause: AttentionCause::Approval,
                evidence: NeedEvidence::Approval {
                    kind: kind.to_string(),
                    detail: command_detail(&payload.command),
                },
                at,
            });
            return;
        }

        match raw.line_type.as_deref() {
            // **The one that names us wins**, unlike every other latest-wins field
            // here. A forked thread — which every subagent rollout is — replays the
            // meta of the thread it forked from into its own history, so a rollout can
            // state several. Only the first is its own: across the corpus the first
            // `session_meta` id matches the id in the filename in 193 of 193 rollouts,
            // the last in 138. Letting the last win hands 55 rollouts their *parent's*
            // id, which for a Sub-agent breaks the chain its own children climb.
            //
            // The guard is "we have no id yet" rather than "we have seen a meta", so
            // the unobserved shape where the first one names none falls through to the
            // next rather than locking the rollout out of an id — and out of a card —
            // for good. Both readings agree on all 193; they differ only there, and
            // there, a possibly-wrong id beats certainly no card.
            Some("session_meta") if self.id.is_none() => {
                if payload.id.is_some() {
                    self.id = payload.id;
                }
                if payload.thread_source.as_deref() == Some("subagent") {
                    self.is_subagent = true;
                }
                if payload.parent_thread_id.is_some() {
                    self.parent_thread_id = payload.parent_thread_id;
                }
                if let Some(depth) = spawn_depth(&payload.source) {
                    self.depth = depth;
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
            Some("event_msg") => match payload.payload_type.as_deref() {
                Some("token_count") => {
                    // Cumulative for the session — overwrite, do not sum.
                    if let Some(usage) = payload.info.and_then(|i| i.total_token_usage) {
                        self.tokens_in = usage.input_tokens;
                        self.tokens_out = usage.output_tokens;
                    }
                }
                // An interrupted / killed turn is an abnormal ending. Codex records
                // no error text on the abort, so evidence is absent. It names no
                // outcome word for it either, so a Sub-agent that ends here ends
                // unworded — and never carries its parent into Attention, since a
                // Sub-agent projection has no Attention to carry (ADR 0010/0014).
                Some("turn_aborted") => {
                    self.lifecycle = Lifecycle::Ended(None);
                    self.attention.apply(Observation::Need {
                        key: "error".into(),
                        cause: AttentionCause::Error,
                        evidence: NeedEvidence::Error { text: None },
                        at,
                    })
                }
                // A turn starting or completing cleanly is forward progress and
                // supersedes any prior error/approval (recovery). For a Sub-agent
                // `task_complete` is also the terminal event — the one record that
                // says it is done — and a `task_started` after one is a resumption,
                // which drops the word and returns the row to Running.
                Some("task_complete") => {
                    self.lifecycle = Lifecycle::Ended(Some(COMPLETED.to_string()));
                    self.attention.apply(Observation::Superseded)
                }
                Some("task_started") => {
                    self.lifecycle = Lifecycle::Running;
                    self.attention.apply(Observation::Superseded)
                }
                _ => {}
            },
            Some("response_item")
                if payload.payload_type.as_deref() == Some("message")
                    && payload.role.as_deref() == Some("assistant") =>
            {
                // Forward progress: an assistant message supersedes any prior
                // error / answered approval.
                self.attention.apply(Observation::Superseded);
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

    fn projection(&self) -> Option<Folded> {
        let id = self.id.clone()?;
        // A subagent rollout is folded like any other and *stated* as a Sub-agent,
        // which is not a card — rather than suppressed into nothing.
        if self.is_subagent {
            return Some(Folded::SubAgent(SubAgentProjection {
                id,
                // Codex names the immediate spawner, which may itself be a Sub-agent;
                // the store walks that chain to the root, and holds this Sub-agent out
                // of every roster if it cannot.
                attachment: self.parent_thread_id.clone().map(Attachment::Spawner),
                // Codex records no per-spawn key: the parent thread id names the
                // spawner, which every sibling shares, so it is the attachment and not
                // this. The row therefore stands on its own id.
                spawn_key: None,
                // **No Errand, and nothing parsed that could become one.** Codex's only
                // fully-covered field is a nickname — `Dirac`, `Euclid` — which names
                // nothing about the work; `agent_path` and `agent_role` are on 59 and
                // 16 of 79 observed rollouts and name a configuration rather than a
                // purpose. An Errand is present when the source states one and absent
                // otherwise, and a blank beats a label that merely looks like content
                // (CONTEXT.md, ADR 0014).
                //
                // So none of the three is modelled in `Payload` at all, deliberately: a
                // field sitting there unread is how the next reader comes to fill this
                // blank with the nearest string to hand. Adding one back is a line of
                // serde away if a Codex version ever states a purpose.
                errand: None,
                depth: self.depth,
                state: match self.lifecycle {
                    Lifecycle::Running => crate::model::SubAgentState::Running,
                    Lifecycle::Ended(_) => crate::model::SubAgentState::Finished,
                },
                outcome: match &self.lifecycle {
                    Lifecycle::Ended(word) => word.clone(),
                    Lifecycle::Running => None,
                },
                tool: Tool::Codex,
                model: self.model.clone(),
                tokens_in: self.tokens_in,
                tokens_out: self.tokens_out,
                cost_usd: crate::pricing::estimate_cost_usd(
                    self.model.as_deref(),
                    self.tokens_in,
                    self.tokens_out,
                )
                .unwrap_or(0.0),
                last_event_at: self.latest_timestamp,
            }));
        }
        Some(Folded::AgentSession(Projection {
            id,
            tool: Tool::Codex,
            project: self.project.clone().unwrap_or_default(),
            model: self.model.clone(),
            branch: self.branch.clone(),
            cwd: self.cwd.clone(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            // A Codex Agent Session's own Sub-agents are rollouts of their own, folded
            // separately and joined onto this roster by the store, which is the only
            // place that sees both files. Codex records no spawn of its own in the
            // parent rollout, so this side of the union contributes nothing and every
            // row arrives from a child.
            sub_agent_roster: Vec::new(),
            activity: self.activity.clone(),
            last_event_at: self.latest_timestamp,
            attention: self.attention.current(),
        }))
    }
}

/// The spawn depth a `session_meta` states for a subagent rollout — 1 for a
/// Sub-agent of an Agent Session, 2 for one a Sub-agent spawned itself — or `None`
/// when it states none. An absence, not a level: 4 of 79 observed subagent rollouts
/// carry no spawn block at all.
///
/// Dug out of an untyped `Value` rather than deserialized into a struct, because
/// `session_meta.source` is **polymorphic**: an object holding the spawn block on a
/// subagent rollout, and a bare string naming the originator on a plain one (169 of
/// 252 observed). A typed field would reject the string shape — and rejecting *that*
/// line is rejecting the `session_meta`, which would cost the rollout its id and an
/// ordinary Codex session its card. The lenient read fails to nothing worse than a
/// missing depth.
fn spawn_depth(source: &Option<serde_json::Value>) -> Option<u32> {
    let depth = source
        .as_ref()?
        .get("subagent")?
        .get("thread_spawn")?
        .get("depth")?
        .as_u64()?;
    u32::try_from(depth).ok()
}

/// The structured approval-kind label for a Codex event name, or `None` when the
/// event is not an approval request.
///
/// The expected markers per issue #7 are `exec_approval_request` /
/// `apply_patch_approval_request`. **Unverified against a live run**: every local
/// rollout ran `approval_policy: never`, so no approval was ever observed. These
/// names are unambiguous (a `*_approval_request` cannot mean anything else), so
/// there is no false-positive risk on normal runs; if a real approval-gated CLI
/// emits a differently-named event, this simply does not fire and the session
/// stays out of Attention (a safe degradation, never a crash or false alert). The
/// secondary "unanswered `function_call` at the tail" proxy from the issue is
/// deliberately *not* used: a tool call is momentarily unanswered on every normal
/// turn, which would resurrect exactly the false-Attention noise C3 removes. The
/// returned label is a structured, allowlisted field — safe to cross the wire,
/// unlike the command it gates.
fn approval_kind(name: Option<&str>) -> Option<&'static str> {
    match name {
        Some("exec_approval_request") => Some("exec"),
        Some("apply_patch_approval_request") => Some("apply patch"),
        _ => None,
    }
}

/// A short, source-faithful command detail for local approval evidence: an argv
/// array joined with spaces, or a bare string command. `None` when absent. This is
/// local-only — it never crosses the wire (only the [`approval_kind`] does).
fn command_detail(command: &Option<serde_json::Value>) -> Option<String> {
    match command {
        Some(serde_json::Value::Array(parts)) => {
            let joined = parts
                .iter()
                .filter_map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            first_line(&joined)
        }
        Some(serde_json::Value::String(s)) => first_line(s),
        _ => None,
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
    use crate::model::Session;
    use crate::session::FileState;
    use crate::source::SessionSource;

    fn codex() -> FileState {
        FileState::new(Box::new(CodexFold::default()))
    }

    /// The cause of a session's current Attention, or `None`.
    fn cause(s: &Session) -> Option<AttentionCause> {
        s.attention.as_ref().map(|a| a.cause)
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

    /// A subagent rollout's `session_meta`, in the shape the corpus writes it: the
    /// spawner named at the top level *and* inside the spawn block, the depth only
    /// inside it, and a nickname that names nothing about the work.
    fn subagent_meta(id: &str, parent: &str, depth: u32) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "session_id": parent,
                "parent_thread_id": parent,
                "cwd": "/Users/x/repos/foo",
                "thread_source": "subagent",
                "agent_nickname": "Dirac",
                "agent_path": "/root/spec_review",
                "source": { "subagent": { "thread_spawn": {
                    "parent_thread_id": parent,
                    "depth": depth,
                    "agent_path": "/root/spec_review",
                    "agent_nickname": "Dirac",
                    "agent_role": null
                }}},
                "git": { "branch": "main" }
            }
        })
        .to_string()
    }

    /// The Sub-agent projection a run of lines folds to, or a panic naming what it
    /// produced instead.
    fn sub_agent_of(lines: &[String]) -> SubAgentProjection {
        let mut fold = CodexFold::default();
        for line in lines {
            assert!(fold.apply_line(line), "line did not parse: {line}");
        }
        match fold.projection() {
            Some(Folded::SubAgent(sub)) => sub,
            other => panic!("expected a Sub-agent projection, got {other:?}"),
        }
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
        let s = fs
            .build(ts("2026-07-19T10:01:00Z"), ts("2026-07-19T10:02:00Z"))
            .unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tool, Tool::Codex);
        assert_eq!(s.project, "foo");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(s.tokens_in, 1000);
        assert_eq!(s.tokens_out, 200);
        assert_eq!(s.activity.as_deref(), Some("wiring the collector"));
    }

    /// An `event_msg` carrying a bare lifecycle `payload.type`.
    fn event(payload_type: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:05Z",
            "type": "event_msg",
            "payload": { "type": payload_type, "turn_id": "t1" }
        })
        .to_string()
    }

    /// An approval request as its own top-level line `type`.
    fn approval(line_type: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-07-19T10:00:06Z",
            "type": line_type,
            "payload": { "call_id": "c1", "command": ["rm", "-rf", "x"] }
        })
        .to_string()
    }

    #[test]
    fn turn_aborted_is_attention_error() {
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                event("task_started"),
                event("turn_aborted"),
            ],
        );
        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, crate::model::Status::Attention);
        assert_eq!(cause(&s), Some(AttentionCause::Error));
    }

    #[test]
    fn task_complete_and_quiet_is_finished() {
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                event("task_started"),
                event("task_complete"),
            ],
        );
        // 30 min quiet, cleanly completed → Finished, never Attention.
        let s = fs
            .build(ts("2026-07-19T10:00:00Z"), ts("2026-07-19T10:30:00Z"))
            .unwrap();
        assert_eq!(s.status, crate::model::Status::Finished);
        assert_eq!(cause(&s), None);
    }

    #[test]
    fn task_started_fresh_is_active() {
        let mut fs = codex();
        feed(&mut fs, &[meta("rollout-1", "user"), event("task_started")]);
        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, crate::model::Status::Active);
        assert_eq!(cause(&s), None);
    }

    #[test]
    fn approval_request_is_attention_waiting_and_clears() {
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                event("task_started"),
                approval("exec_approval_request"),
            ],
        );
        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, crate::model::Status::Attention);
        assert_eq!(cause(&s), Some(AttentionCause::Approval));

        // Answering it (forward progress) drops the card out of Attention.
        feed(&mut fs, &[assistant_message("running the command")]);
        let s = fs
            .build(ts("2026-07-19T10:04:30Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, crate::model::Status::Active);
        assert_eq!(cause(&s), None);
    }

    #[test]
    fn activity_after_abort_clears_attention() {
        // Recovery (story 19): a new turn after an abort leaves the error state.
        let mut fs = codex();
        feed(
            &mut fs,
            &[
                meta("rollout-1", "user"),
                event("turn_aborted"),
                event("task_started"),
            ],
        );
        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        assert_eq!(s.status, crate::model::Status::Active);
        assert_eq!(cause(&s), None);
    }

    #[test]
    fn subagent_rollout_yields_no_card() {
        let mut fs = codex();
        feed(&mut fs, &[meta("sub-1", "subagent"), token_count(999, 999)]);
        assert!(fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .is_none());
    }

    #[test]
    fn a_subagent_rollout_states_a_sub_agent_projection() {
        // A subagent rollout is not path-distinguishable (same directory, same
        // `rollout-` name), so Codex classifies it from `thread_source` — but it
        // reports the same stated outcome as Claude: a Sub-agent, not nothing.
        let path = std::path::Path::new("/x/2026/07/20/rollout-2026-07-20T13-39-51-sub.jsonl");
        let mut fold = crate::source::CodexSource::new("/x".into()).new_fold(path);
        for line in [
            subagent_meta("sub-1", "root-1", 1),
            turn_context("gpt-5.6-sol"),
            token_count(1000, 200),
        ] {
            assert!(fold.apply_line(&line));
        }
        let Some(Folded::SubAgent(sub)) = fold.projection() else {
            panic!("a subagent rollout states a Sub-agent projection");
        };
        assert_eq!(sub.id, "sub-1");
        assert_eq!(sub.tool, Tool::Codex);
        assert_eq!(sub.model.as_deref(), Some("gpt-5.6-sol"));
        // The spend that was being attributed to nothing: 227M input tokens across the
        // corpus, from the same cumulative-latest rule the Agent Session side uses.
        assert_eq!(sub.tokens_in, 1000);
        assert_eq!(sub.tokens_out, 200);
        // Codex names the immediate spawner; the store walks from there to the root.
        assert_eq!(sub.attachment, Some(Attachment::Spawner("root-1".into())));
        assert_eq!(sub.depth, 1);
    }

    #[test]
    fn a_codex_sub_agent_carries_no_errand_and_no_spawn_key() {
        // The nickname (`Dirac`), the agent path and the role are all read past — none
        // names what the Sub-agent was sent to do, and a blank beats a label that
        // merely looks like content. The parent thread id names the *spawner*, which
        // every sibling shares, so it is the attachment rather than a per-spawn key:
        // putting it in `spawn_key` would collide all of one parent's children onto a
        // single row.
        let sub = sub_agent_of(&[subagent_meta("sub-1", "root-1", 1), token_count(10, 2)]);
        assert_eq!(sub.errand, None);
        assert_eq!(sub.spawn_key, None);
        // And the row it becomes stands under its own id rather than joining a spawn.
        let row = sub.roster_entry();
        assert_eq!(row.errand, None);
        assert_eq!(row.spawn_key, "sub-1");
    }

    #[test]
    fn the_terminal_event_finishes_a_codex_sub_agent_with_the_word_codex_states() {
        let sub = sub_agent_of(&[
            subagent_meta("sub-1", "root-1", 1),
            token_count(1000, 200),
            event("task_started"),
            event("task_complete"),
        ]);
        assert_eq!(sub.outcome.as_deref(), Some("completed"));
        let row = sub.roster_entry();
        assert_eq!(row.state, crate::model::SubAgentState::Finished);
        assert_eq!(row.outcome.as_deref(), Some("completed"));
    }

    #[test]
    fn a_codex_sub_agent_that_ends_any_other_way_ends_unworded() {
        // Codex names one outcome word and only one. An aborted turn ends a Sub-agent
        // just as truly as a completed one and Codex has no word for it, so the row
        // stops rather than carrying an invented word — an ending is not a word.
        //
        // Ending it is the point: a row that claimed to still be running would hold its
        // parent out of Finished for as long as the parent lived, since a Running
        // Sub-agent is exactly what keeps a quiet parent Active. 1 of 79 observed
        // rollouts ends this way, and one is enough to pin a card forever.
        let sub = sub_agent_of(&[
            subagent_meta("sub-1", "root-1", 1),
            event("task_started"),
            event("turn_aborted"),
        ]);
        assert_eq!(sub.outcome, None);
        assert_eq!(
            sub.roster_entry().state,
            crate::model::SubAgentState::Finished
        );
        // And it raises nothing on the parent: a Sub-agent projection carries no
        // Attention, so a failure is reported to the agent, never to the person.
        assert!(matches!(
            sub_agent_of(&[subagent_meta("sub-2", "root-1", 1), event("turn_aborted"),]),
            SubAgentProjection { outcome: None, .. }
        ));
    }

    #[test]
    fn a_resumed_codex_sub_agent_returns_to_running_without_a_word() {
        // A Sub-agent can resume after finishing, so the roster carries the latest word
        // rather than a final one: 39 of 79 observed rollouts complete more than once.
        let mut lines = vec![
            subagent_meta("sub-1", "root-1", 1),
            event("task_started"),
            event("task_complete"),
        ];
        assert_eq!(sub_agent_of(&lines).outcome.as_deref(), Some("completed"));

        lines.push(event("task_started"));
        let resumed = sub_agent_of(&lines);
        assert_eq!(resumed.outcome, None);
        assert_eq!(
            resumed.roster_entry().state,
            crate::model::SubAgentState::Running
        );

        lines.push(event("task_complete"));
        assert_eq!(sub_agent_of(&lines).outcome.as_deref(), Some("completed"));
    }

    #[test]
    fn a_subagent_rollout_naming_no_spawner_claims_no_attachment() {
        // Nothing to walk from, so the store holds it out of every roster rather than
        // attaching it to a guess.
        let sub = sub_agent_of(&[meta("sub-1", "subagent"), token_count(10, 2)]);
        assert_eq!(sub.attachment, None);
        // And a rollout with a spawner but no spawn block states no depth — an
        // absence, not a level (4 of 79 observed rollouts).
        let no_block = serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "sub-2", "cwd": "/a/foo",
                "thread_source": "subagent", "parent_thread_id": "root-1"
            }
        })
        .to_string();
        let sub = sub_agent_of(&[no_block]);
        assert_eq!(sub.attachment, Some(Attachment::Spawner("root-1".into())));
        assert_eq!(sub.depth, 0);
    }

    #[test]
    fn a_replayed_ancestor_session_meta_does_not_rename_the_rollout() {
        // A forked thread — which every subagent rollout is — replays the meta of the
        // thread it forked from into its own history, so the *first* `session_meta` is
        // the only one that says who this rollout is (193 of 193 first ids match the
        // filename's; 138 of 193 last ones do). Taking the last would give 55 rollouts
        // their parent's id — and a Sub-agent under its parent's id is a broken link in
        // the chain its own children climb, which is how one of them lost its root.
        let sub = sub_agent_of(&[
            subagent_meta("sub-1", "root-1", 1),
            meta("root-1", "user"),
            token_count(1000, 200),
        ]);
        assert_eq!(sub.id, "sub-1");
        assert_eq!(sub.attachment, Some(Attachment::Spawner("root-1".into())));
    }

    #[test]
    fn a_session_meta_whose_source_is_a_string_still_yields_a_card() {
        // `session_meta.source` is polymorphic — the spawn block on a subagent rollout,
        // a bare originator string on a plain one (169 of 252 observed). Reading it
        // through a typed field would fail *this* line, and failing the `session_meta`
        // costs an ordinary Codex session its whole card.
        let mut fs = codex();
        let meta = serde_json::json!({
            "timestamp": "2026-07-19T10:00:00Z",
            "type": "session_meta",
            "payload": { "id": "rollout-1", "cwd": "/a/foo", "source": "vscode" }
        })
        .to_string();
        feed(&mut fs, &[meta, token_count(42, 7)]);
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tokens_in, 42);
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
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
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
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
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
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
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
        feed(
            &mut fs,
            &[meta("rollout-1", "user"), token_count(50, 5), null_tc],
        );
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
        assert_eq!(s.tokens_in, 50); // last real count retained
    }

    #[test]
    fn unknown_types_and_fields_are_ignored() {
        let mut fs = codex();
        let weird = r#"{"timestamp":"2026-07-19T10:00:05Z","type":"future_event","payload":{"whatever":1},"extra":true}"#.to_string();
        feed(
            &mut fs,
            &[meta("rollout-1", "user"), weird, token_count(7, 1)],
        );
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tokens_in, 7);
    }

    #[test]
    fn no_session_meta_yields_no_card() {
        let mut fs = codex();
        // token_count before any session_meta: no id, so no card.
        feed(&mut fs, &[token_count(10, 2)]);
        assert!(fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .is_none());
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
        let s = fs
            .build(ts("2026-07-19T10:00:10Z"), ts("2026-07-19T10:00:20Z"))
            .unwrap();
        assert_eq!(s.id, "rollout-1");
        assert_eq!(s.tokens_in, 9);
    }
}
