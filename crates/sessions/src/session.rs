//! Two things live here: the Claude Code [`Fold`] ([`Accumulator`]) and the shared
//! incremental tail ([`FileState`]) that drives any source's fold.
//!
//! [`Accumulator`] decodes Claude Code transcript entries and folds them into a
//! [`Projection`]. [`FileState`] is source-agnostic: it owns the byte-offset
//! bookkeeping, truncation reset, malformed-line handling, and the final
//! [`Session`] assembly (status via [`status_for`]) for whatever [`Fold`] it holds.

use chrono::{DateTime, Utc};
use tracing::warn;

use crate::attention::{AttentionReducer, NeedEvidence, Observation};
use crate::fold::{project_from_cwd, status_for, Fold, Projection};
use crate::liveness::ProcessLiveness;
use crate::model::{Attention, AttentionCause, Session, Status, SubAgents, Tool};
use crate::parse::{parse_entry, Entry, ToolUseInfo};

/// The Claude Code `Task` tool spawns a Sub-agent; its tool-use `id` correlates the
/// matching `tool_result` that ends it, and its input `description` names the work.
const TASK_TOOL: &str = "Task";

/// The Claude Code fold: a running projection of a single transcript. Fold entries
/// in file order via [`Accumulator::apply`]; token counts and "latest" fields
/// update in place. Attention lifecycle is delegated entirely to the shared
/// [`AttentionReducer`] — this fold only *translates* each entry into normalized
/// need/resolution [`Observation`]s.
#[derive(Debug, Default)]
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
    /// Sub-agent (sidechain) assistant usage, summed apart from the main counts so
    /// its cost can be priced per the Sub-agent's own (possibly cheaper) model.
    sub_tokens_in: u64,
    sub_tokens_out: u64,
    /// Running cost of Sub-agent usage, priced per each sidechain assistant entry's
    /// own model as it is folded in (not deferred to a single session-model price).
    sub_agent_cost_usd: f64,
    /// Sub-agents currently running, in spawn order: each a `Task` tool-use `id` and
    /// the short description from its input. An entry is pushed when the `Task` spawns
    /// and removed when its matching `tool_result` arrives, so this is always the
    /// *active* set.
    active_sub_agents: Vec<(String, Option<String>)>,
    attention: AttentionReducer,
}

impl Accumulator {
    /// Fold one already-parsed relevant entry into the projection.
    pub fn apply(&mut self, entry: Entry) {
        if let Some(ts) = entry.timestamp {
            // Entries arrive in file (chronological) order, but guard anyway. Every
            // entry — including Sub-agent (sidechain) traffic — bumps recency, so a
            // parent whose own loop is quiet while Sub-agents grind still looks alive.
            if self.latest_timestamp.is_none_or(|cur| ts >= cur) {
                self.latest_timestamp = Some(ts);
            }
        }

        // Sub-agent (sidechain) traffic folds into the parent, never a card of its
        // own: its assistant usage adds to the parent's tokens and to cost — priced
        // per *this* entry's model, since a Sub-agent may run a cheaper one. Its
        // model, activity, id, and attention lifecycle never touch the parent (the
        // activity line stays the orchestrator's own words).
        if entry.is_sidechain {
            if entry.is_assistant {
                self.sub_tokens_in += entry.input_tokens;
                self.sub_tokens_out += entry.output_tokens;
                if let Some(cost) = crate::pricing::estimate_cost_usd(
                    entry.model.as_deref(),
                    entry.input_tokens,
                    entry.output_tokens,
                ) {
                    self.sub_agent_cost_usd += cost;
                }
            }
            return;
        }

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

        // Attention Since prefers the entry's own timestamp, falling back to the
        // latest one seen so far (set just above) when a record carries none.
        let at = entry.timestamp.or(self.latest_timestamp);

        if entry.is_api_error {
            // A synthetic API-error record (model `<synthetic>`, no real content):
            // raise a Session error but never overwrite the card's model/activity.
            self.attention.apply(Observation::Need {
                key: "error".into(),
                cause: AttentionCause::Error,
                evidence: NeedEvidence::Error {
                    text: entry.activity,
                },
                at,
            });
        } else if entry.is_assistant {
            if entry.model.is_some() {
                self.model = entry.model;
            }
            if entry.activity.is_some() {
                self.activity = entry.activity;
            }
            // A `Task` tool-use spawns a Sub-agent: register it as active fan-out. It
            // is *not* a human-input wait (the Sub-agent runs on its own), so it is
            // kept out of the awaiting decision below — otherwise a fanning-out turn
            // would masquerade as needing attention, the exact false pull we remove.
            let mut human_waits: Vec<ToolUseInfo> = Vec::new();
            for tool in entry.tool_uses {
                if tool.name.as_deref() == Some(TASK_TOOL) {
                    if let Some(id) = tool.id {
                        if !self.active_sub_agents.iter().any(|(sid, _)| *sid == id) {
                            self.active_sub_agents.push((id, tool.detail));
                        }
                    }
                } else {
                    human_waits.push(tool);
                }
            }
            // Waiting-on-human = the turn ended to call a (non-`Task`) tool. Prefer the
            // explicit `stop_reason`; fall back to the presence of a `tool_use` block
            // only when `stop_reason` is absent. This stops a mid-run tool_use or a
            // cleanly-ended (`end_turn`) turn from masquerading as a wait.
            let awaiting = match entry.stop_reason.as_deref() {
                Some("tool_use") => !human_waits.is_empty(),
                Some(_) => false,
                None => !human_waits.is_empty(),
            };
            if awaiting {
                // The trailing tool_use is the one the turn is blocked on. Claude
                // transcripts do not mark *why* a call sits unanswered (approval vs
                // mid-run), so the cause is the honest generic fallback rather than
                // an inferred Approval (ADR 0010: keep the card honest and generic).
                let tool = human_waits.into_iter().next_back().unwrap_or_default();
                self.attention.apply(Observation::Need {
                    key: tool.id.unwrap_or_else(|| "tool".into()),
                    cause: AttentionCause::Input,
                    evidence: NeedEvidence::Tool {
                        name: tool.name.unwrap_or_default(),
                        detail: tool.detail,
                    },
                    at,
                });
            } else {
                // A cleanly-ended turn is forward progress: it supersedes any
                // pending error/approval (recovery needs no extra bookkeeping).
                self.attention.apply(Observation::Superseded);
            }
        } else if entry.tool_result_ids.is_empty() {
            // A plain user turn resuming after an error/approval is forward progress.
            self.attention.apply(Observation::Superseded);
        } else {
            // A tool_result answers its tool_use by id — a correlated resolution. If
            // that id spawned a Sub-agent (a `Task`), the Sub-agent is now done, so it
            // leaves the active set and the parent's badge count drops.
            for id in entry.tool_result_ids {
                self.active_sub_agents.retain(|(sid, _)| *sid != id);
                self.attention.apply(Observation::Resolved { key: id });
            }
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
            sub_tokens_in: self.sub_tokens_in,
            sub_tokens_out: self.sub_tokens_out,
            sub_agent_cost_usd: self.sub_agent_cost_usd,
            sub_agents: SubAgents {
                active: self.active_sub_agents.len(),
                descriptions: self
                    .active_sub_agents
                    .iter()
                    .filter_map(|(_, desc)| desc.clone())
                    .collect(),
            },
            activity: self.activity.clone(),
            last_event_at: self.latest_timestamp,
            attention: self.attention.current(),
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

    /// Build the UI [`Session`] without process-liveness data — the mtime rule
    /// alone drives status. Equivalent to
    /// [`build_with_liveness`](Self::build_with_liveness) with
    /// [`ProcessLiveness::Unknown`].
    pub fn build(&self, mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<Session> {
        self.build_with_liveness(mtime, now, ProcessLiveness::Unknown)
    }

    /// Build the UI [`Session`], given the file's mtime and the session's process
    /// verdict (together they drive status — see [`status_for`]). Returns `None`
    /// if the fold has no projection yet.
    pub fn build_with_liveness(
        &self,
        mtime: DateTime<Utc>,
        now: DateTime<Utc>,
        liveness: ProcessLiveness,
    ) -> Option<Session> {
        let p = self.fold.projection()?;
        let status = status_for(
            p.attention.is_some(),
            p.sub_agents.active > 0,
            mtime,
            now,
            liveness,
        );
        // Resolve the current need into the atomic `attention` value only when the
        // status is actually Attention, so the two can never disagree on the wire.
        // Attention Since falls back to the file mtime when the source recorded no
        // timestamp for the need.
        let attention = (status == Status::Attention)
            .then_some(p.attention)
            .flatten()
            .map(|a| Attention {
                cause: a.cause,
                // Attention survives process death (the wait still needs a human),
                // but the card must not pretend the session is resumable in place:
                // a local, factual note rides after the source-faithful evidence.
                evidence: match (a.evidence, liveness == ProcessLiveness::Dead) {
                    (Some(e), true) => Some(format!("{e} · process exited")),
                    (None, true) => Some("process exited".to_string()),
                    (e, false) => e,
                },
                since: a.since.unwrap_or(mtime),
                details_on_source: false,
                remote_evidence: a.remote_evidence,
            });
        // Cost is pure (tokens × the model's list price). The main-conversation usage
        // is priced at the session model; the Sub-agent usage was already priced per
        // each Sub-agent's own model as it was folded (they may run cheaper models),
        // so the two are summed here. When the main model is unpriced but Sub-agents
        // ran priced models, the card still shows their cost rather than nothing. The
        // live git `diff` is out-of-transcript, so the board fills it and the sessions
        // projection leaves it None.
        let main_cost =
            crate::pricing::estimate_cost_usd(p.model.as_deref(), p.tokens_in, p.tokens_out);
        let cost_usd = match main_cost {
            Some(main) => Some(main + p.sub_agent_cost_usd),
            None if p.sub_agent_cost_usd > 0.0 => Some(p.sub_agent_cost_usd),
            None => None,
        };
        // The card's token counts include Sub-agent usage — fan-out spend is real and
        // counted (the split above exists only so cost could be priced per model).
        let tokens_in = p.tokens_in + p.sub_tokens_in;
        let tokens_out = p.tokens_out + p.sub_tokens_out;
        Some(Session {
            id: p.id,
            tool: p.tool,
            project: p.project,
            model: p.model,
            branch: p.branch,
            cwd: p.cwd,
            tokens_in,
            tokens_out,
            activity: p.activity,
            last_event_at: p.last_event_at.unwrap_or(mtime),
            status,
            attention,
            cost_usd,
            diff: None,
            sub_agents: p.sub_agents,
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
    use crate::model::{AttentionCause, Status};

    /// The cause of a session's current Attention, or `None`.
    fn cause(s: &Session) -> Option<AttentionCause> {
        s.attention.as_ref().map(|a| a.cause)
    }

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

    /// A main-chain assistant turn that spawns a Sub-agent via a `Task` tool-use,
    /// carrying its correlation id and a short description.
    fn task_spawn(tuid: &str, description: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{{"model":"claude-opus-4-8","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"{tuid}","name":"Task","input":{{"description":"{description}","subagent_type":"Explore"}}}}]}}}}"#
        )
    }

    /// A `tool_result` (main chain) answering the `Task` with id `tuid` — the
    /// Sub-agent has finished.
    fn task_result(tuid: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"s1","cwd":"/a/foo","message":{{"content":[{{"type":"tool_result","tool_use_id":"{tuid}","content":"done"}}]}}}}"#
        )
    }

    /// One Sub-agent's own (sidechain) assistant turn, on an arbitrary model.
    fn sidechain(model: &str, text: &str, tin: u64, tout: u64) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":true,"sessionId":"sub","cwd":"/a/b","timestamp":"2026-07-19T10:00:10Z","message":{{"model":"{model}","usage":{{"input_tokens":{tin},"output_tokens":{tout}}},"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn active_sub_agent_count_rises_on_task_and_falls_on_result() {
        let mut fs = claude();
        // Two Task spawns → two active Sub-agents, each with its description.
        let mut data = task_spawn("toolu_a", "map the parser");
        data.push('\n');
        data.push_str(&task_spawn("toolu_b", "audit the tests"));
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:20Z"), ts("2026-07-19T10:00:30Z"))
            .unwrap();
        assert_eq!(s.sub_agents.active, 2);
        assert_eq!(
            s.sub_agents.descriptions,
            vec!["map the parser".to_string(), "audit the tests".to_string()]
        );

        // The first Sub-agent's tool_result arrives → count drops to one.
        fs.feed(format!("{}\n", task_result("toolu_a")).as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:40Z"), ts("2026-07-19T10:00:50Z"))
            .unwrap();
        assert_eq!(s.sub_agents.active, 1);
        assert_eq!(s.sub_agents.descriptions, vec!["audit the tests".to_string()]);

        // The second completes → no badge.
        fs.feed(format!("{}\n", task_result("toolu_b")).as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:55Z"), ts("2026-07-19T10:01:00Z"))
            .unwrap();
        assert_eq!(s.sub_agents.active, 0);
        assert!(s.sub_agents.descriptions.is_empty());
    }

    #[test]
    fn spawning_a_sub_agent_is_working_not_attention_or_stale() {
        // A `Task` spawn ends the turn with stop_reason tool_use, but it is the agent
        // fanning out — not a human-input wait — so the card must not enter Attention.
        // And with a Sub-agent still active the parent stays Active even when its own
        // transcript has been quiet well past the staleness window.
        let mut fs = claude();
        fs.feed(format!("{}\n", task_spawn("toolu_a", "grind on it")).as_bytes());
        let s = fs
            .build(ts("2026-07-19T10:00:00Z"), ts("2026-07-19T10:30:00Z"))
            .unwrap();
        assert_eq!(s.status, Status::Active);
        assert!(s.attention.is_none());
        assert_eq!(s.sub_agents.active, 1);
    }

    #[test]
    fn sidechain_usage_folds_into_tokens_and_cost_priced_per_entry_model() {
        // Orchestrator on Opus; its Sub-agent runs the cheaper Haiku. Both usages
        // count toward the card, but the Sub-agent's cost is priced at Haiku's rate,
        // not the parent's Opus rate.
        let mut fs = claude();
        let mut data = assistant("s1", "orchestrating", 1_000_000, 1_000_000);
        data.push('\n');
        data.push_str(&sidechain("claude-haiku-4-5", "sub work", 1_000_000, 0));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        // Tokens include the Sub-agent's usage (story 5).
        assert_eq!(s.tokens_in, 2_000_000);
        assert_eq!(s.tokens_out, 1_000_000);
        // Cost = Opus main (15 + 75 = 90) + Haiku sub (0.80), NOT Opus-priced sub (15).
        let cost = s.cost_usd.unwrap();
        assert!((cost - 90.80).abs() < 1e-9, "cost was {cost}");
        // Model stays the orchestrator's, never the Sub-agent's.
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn sidechain_text_and_timestamp_bump_recency_but_not_activity() {
        // The activity line stays the orchestrator's own words (story 7), while the
        // later sidechain timestamp still bumps the parent's recency (story 4/9).
        let mut fs = claude();
        let mut data = assistant("s1", "orchestrating", 10, 1);
        data.push('\n');
        data.push_str(&sidechain("claude-haiku-4-5", "noisy sub chatter", 5, 1));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        assert_eq!(s.activity.as_deref(), Some("orchestrating"));
        // last_event_at is the sidechain entry's timestamp, the latest seen.
        assert_eq!(s.last_event_at, ts("2026-07-19T10:00:10Z"));
    }

    #[test]
    fn sub_agent_cost_shows_even_when_the_main_model_is_unpriced() {
        // An unknown orchestrator model has no main cost, but a priced Sub-agent's
        // spend still surfaces rather than vanishing.
        let mut fs = claude();
        let unknown = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{"model":"some-future-model","usage":{"input_tokens":10,"output_tokens":1},"content":[{"type":"text","text":"go"}]}}"#;
        let mut data = unknown.to_string();
        data.push('\n');
        data.push_str(&sidechain("claude-haiku-4-5", "sub", 1_000_000, 0));
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:00:30Z"), ts("2026-07-19T10:00:40Z"))
            .unwrap();
        let cost = s.cost_usd.expect("Sub-agent cost surfaces");
        assert!((cost - 0.80).abs() < 1e-9, "cost was {cost}");
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
        assert_eq!(cause(&s), Some(AttentionCause::Input));
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
        assert_eq!(cause(&s), None);
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
        assert_eq!(cause(&s), Some(AttentionCause::Error));
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
        assert_eq!(cause(&s), Some(AttentionCause::Input));
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
        assert_eq!(cause(&s), None);
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
    fn pending_tool_use_carries_input_cause_and_local_evidence() {
        // A tool the agent is blocked on: honest generic cause, source-faithful
        // local evidence (tool name + a recognized input field), Since = the call's
        // own timestamp.
        let mut fs = claude();
        let line = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"cargo test --workspace"}}]}}"#;
        let mut data = line.to_string();
        data.push('\n');
        fs.feed(data.as_bytes());

        let s = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap();
        let a = s.attention.unwrap();
        assert_eq!(a.cause, AttentionCause::Input);
        assert_eq!(a.since, ts("2026-07-19T10:00:00Z"));
        assert_eq!(a.evidence.as_deref(), Some("Bash: cargo test --workspace"));
        // The wire rendering keeps the tool name but never its arguments.
        assert_eq!(a.remote_evidence.as_deref(), Some("Bash"));
    }

    #[test]
    fn a_newer_need_replaces_the_current_one_and_resets_since() {
        // Two different pending tool calls in sequence: the card describes the
        // current need, and Attention Since jumps to the newer call (Story 8).
        let mut fs = claude();
        let first = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:00:00Z","message":{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_1","name":"Read"}]}}"#;
        let result = r#"{"type":"user","sessionId":"s1","cwd":"/a/foo","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}"#;
        let second = r#"{"type":"assistant","sessionId":"s1","cwd":"/a/foo","timestamp":"2026-07-19T10:03:00Z","message":{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_2","name":"Bash"}]}}"#;
        let mut data = String::new();
        for l in [first, result, second] {
            data.push_str(l);
            data.push('\n');
        }
        fs.feed(data.as_bytes());

        let a = fs
            .build(ts("2026-07-19T10:04:00Z"), ts("2026-07-19T10:05:00Z"))
            .unwrap()
            .attention
            .unwrap();
        assert_eq!(a.since, ts("2026-07-19T10:03:00Z"));
        assert_eq!(a.evidence.as_deref(), Some("Bash"));
    }

    #[test]
    fn alive_process_overrides_a_stale_mtime() {
        // The board's Running band is ground truth: a live agent whose transcript
        // has been quiet past the window is still Active.
        let mut fs = claude();
        let mut data = assistant("s1", "thinking", 1, 1);
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build_with_liveness(
                ts("2026-07-19T10:00:00Z"),
                ts("2026-07-19T10:25:00Z"),
                crate::liveness::ProcessLiveness::Alive,
            )
            .unwrap();
        assert_eq!(s.status, Status::Active);
    }

    #[test]
    fn dead_process_overrides_a_fresh_mtime() {
        // The Ctrl-C false positive: file fresh, process gone → Finished.
        let mut fs = claude();
        let mut data = assistant("s1", "was working", 1, 1);
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build_with_liveness(
                ts("2026-07-19T10:04:00Z"),
                ts("2026-07-19T10:05:00Z"),
                crate::liveness::ProcessLiveness::Dead,
            )
            .unwrap();
        assert_eq!(s.status, Status::Finished);
    }

    #[test]
    fn attention_survives_process_death_with_annotated_evidence() {
        // An unanswered wait still needs a human even after Ctrl-C; the card says
        // the process exited instead of silently filing under Finished.
        let mut fs = claude();
        let mut data = assistant_tool_use("s1");
        data.push('\n');
        fs.feed(data.as_bytes());
        let s = fs
            .build_with_liveness(
                ts("2026-07-19T10:04:00Z"),
                ts("2026-07-19T10:05:00Z"),
                crate::liveness::ProcessLiveness::Dead,
            )
            .unwrap();
        assert_eq!(s.status, Status::Attention);
        let evidence = s.attention.unwrap().evidence.unwrap();
        assert!(evidence.ends_with("· process exited"), "{evidence}");
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
