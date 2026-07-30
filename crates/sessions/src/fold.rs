//! The Session Source seam. Each tool (Claude Code, Codex CLI) decodes its own
//! transcript lines by implementing [`Fold`]; everything downstream — byte-offset
//! tailing, truncation reset, the mtime-based status heuristic, the [`Session`]
//! shape — is shared and lives in `session.rs`.
//!
//! A [`Fold`] folds a file's lines into one of the two things a transcript can be
//! ([`Folded`]): an Agent Session [`Projection`] — the tool-agnostic inputs the
//! shared builder needs — or a [`SubAgentProjection`]. The status column is derived
//! here from [`Projection::attention`] plus file mtime, so no source reimplements it.

use chrono::{DateTime, Duration, Utc};

use crate::attention::PendingAttention;
use crate::liveness::ProcessLiveness;
use crate::model::{Attention, Session, Status, SubAgent, SubAgentState, Tool};

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

    /// What this fold has produced so far — an Agent Session projection or a
    /// Sub-agent projection — or `None` while nothing has yet supplied an identity.
    ///
    /// A fold *states* which of the two it produced. Neither is reached by an early
    /// return that skips identity assignment: "not a card" and "not folded" are
    /// different things (ADR 0014).
    fn projection(&self) -> Option<Folded>;
}

/// The two things a transcript can be, and which one a [`Fold`] says it produced.
///
/// The distinction holds at the compile boundary rather than by convention. That is
/// the whole reason ADR 0014 chose a distinct type over an Agent Session carrying a
/// parent field: a consumer that would retarget a Work Link or claim a Process
/// Liveness credit is handed a [`SubAgentProjection`], which has no branch and no
/// working directory to read.
#[derive(Debug, Clone, PartialEq)]
pub enum Folded {
    /// An Agent Session — the unit the board displays as a card.
    AgentSession(Projection),
    /// A Sub-agent — folded in full, and never a card, because it carries no
    /// independent human need: nobody can approve, answer, or resume it directly,
    /// only the Agent Session that sent it.
    SubAgent(SubAgentProjection),
}

/// The per-source projection of one Sub-agent's transcript.
///
/// Deliberately *not* card-shaped: no Attention, no branch, no working directory.
/// Those are the fields Work Link and Process Liveness read, and a Sub-agent shares
/// its parent's branch and cwd verbatim — so carrying them is how a Work Item chip
/// would come to point at a card that does not exist, and how a parent would lose
/// its liveness credit to its own children. Absent by type, they cannot.
///
/// `PartialEq` only (not `Eq`): `cost_usd` is an `f64`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubAgentProjection {
    /// The Sub-agent's own source-native id: Claude's `agentId`, Codex's
    /// `session_meta` rollout id.
    pub id: String,
    /// The root Agent Session this Sub-agent belongs to — the only node in the
    /// spawn tree that is a card, however deep the Sub-agent was spawned. `None`
    /// while the source cannot yet resolve it, in which case the Sub-agent is held
    /// out of every roster rather than attached to a guess.
    pub root_session_id: Option<String>,
    /// The key that joins this Sub-agent back to the spawn that created it: Claude's
    /// spawning tool-use id, Codex's parent thread id. `None` when the source states
    /// none, in which case this Sub-agent joins no spawn record and stands as its own
    /// roster row — which is right, since nothing links it to one.
    pub spawn_key: Option<String>,
    /// What it was sent to do, verbatim from the source (bounded where it is read,
    /// like every other string the card carries). `None` when none is stated.
    pub errand: Option<String>,
    /// How deep it was spawned: 1 for a Sub-agent of an Agent Session, 2 for one a
    /// Sub-agent spawned itself. `0` means the source has not stated a depth — both
    /// producers of a Claude row emit 1 for a direct Sub-agent, so a 0 is an absence
    /// rather than a level.
    pub depth: u32,
    pub tool: Tool,
    /// The model this Sub-agent ran, which may be cheaper than the orchestrator's.
    pub model: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Already priced at this Sub-agent's *own* model as it was folded, so the
    /// parent's total is a sum rather than a re-pricing. `0.0` for an unpriced model.
    pub cost_usd: f64,
    /// Latest entry timestamp, or `None` when no timestamped line has arrived.
    pub last_event_at: Option<DateTime<Utc>>,
}

impl SubAgentProjection {
    /// This Sub-agent as a roster row — what it spent, and what the source said it
    /// was sent to do.
    ///
    /// The row's state is `Running`, and its outcome absent: a Claude Sub-agent's
    /// completion is stated in its *parent's* notification records, not in its own
    /// file, so this side of the union never has the word — [`merge_roster`] takes it
    /// from the side that does. A parent that is itself Finished demotes every
    /// still-Running row of its own accord in [`assemble`]. `cost_usd` is `None` for
    /// an unpriced model rather than a misleading `0.00`, matching the folded `0.0`
    /// accumulator's meaning.
    ///
    /// The join key falls back to this Sub-agent's own id when the source states
    /// none: it then matches no spawn record and the row stands on its own.
    pub fn roster_entry(&self) -> SubAgent {
        SubAgent {
            id: self.id.clone(),
            spawn_key: self.spawn_key.clone().unwrap_or_else(|| self.id.clone()),
            errand: self.errand.clone(),
            state: SubAgentState::Running,
            outcome: None,
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            cost_usd: (self.cost_usd > 0.0).then_some(self.cost_usd),
            model: self.model.clone(),
            depth: self.depth,
            last_event_at: self.last_event_at,
        }
    }
}

/// The roster of one Agent Session: the union of what its own transcript recorded
/// and what its Sub-agents' files spent, keyed by the spawn's tool-use id.
///
/// Neither side alone is required for a row. The parent's spawn record establishes
/// that a Sub-agent exists, what it was sent to do, and — for Claude — how it ended,
/// which its notifications state; it is there the instant the spawn is written,
/// before any child file exists. The child file supplies what that Sub-agent spent,
/// and is the *only* side for a Sub-agent spawned by another Sub-agent, whose spawn
/// was recorded in a child transcript rather than the parent's (1 of 70 observed
/// children).
///
/// `spawns` keeps its order — that is spawn order, which is what the roster reads in.
/// Rows contributed by a child file alone are appended in a deterministic order
/// (oldest first, ties broken by id) rather than in whatever order the store's file
/// map happened to yield, so an unchanged roster compares equal and the store's
/// no-op suppression holds.
///
/// **A spawn key identifies at most one Sub-agent.** Claude mints one per `Agent`
/// tool-use and its sidecar carries it back, so two children cannot honestly claim
/// the same one; a Sub-agent resumed after finishing keeps writing to its own file
/// under its own `agentId`. If two ever did collide, the later one would take the
/// row and the earlier one's spend would leave the card silently — stated here
/// because the totals are derived from this roster, so a dropped row is a dropped
/// number, and pinned by `two_contributions_cannot_honestly_share_a_spawn_key`.
pub fn merge_roster(mut spawns: Vec<SubAgent>, contributions: Vec<SubAgent>) -> Vec<SubAgent> {
    let mut extra: Vec<SubAgent> = Vec::new();
    for child in contributions {
        match spawns.iter_mut().find(|s| s.spawn_key == child.spawn_key) {
            // The spawn said what the Sub-agent was for; the child says what it cost.
            // Errand stays the parent's word when it stated one — both sides copy it
            // from the same spawn, so this only decides which copy is read.
            //
            // How it *ended* comes from whichever side the source stated it on, and the
            // sides differ by tool: Claude's completion is a notification in the
            // parent's transcript, Codex's a terminal marker in the child's own
            // rollout. Taking the child's row wholesale would put a Sub-agent Claude
            // has already reported finished back to Running on its next folded line.
            Some(row) => {
                let errand = row.errand.take().or(child.errand);
                let (state, outcome) = match row.outcome.take() {
                    Some(word) => (row.state, Some(word)),
                    None => (child.state, child.outcome),
                };
                *row = SubAgent {
                    errand,
                    state,
                    outcome,
                    ..child
                };
            }
            None => extra.push(child),
        }
    }
    extra.sort_by(|a, b| {
        a.last_event_at
            .cmp(&b.last_event_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    spawns.extend(extra);
    spawns
}

/// Tool-agnostic inputs the shared builder turns into a [`Session`]. Everything
/// except `status`, which the builder computes from `pending_input` and mtime.
///
/// `PartialEq` only (not `Eq`): the roster's `cost_usd` is an `f64`.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub id: String,
    pub tool: Tool,
    pub project: String,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    /// This session's *own* assistant token usage. Its Sub-agents' spend lives on
    /// the roster, priced per each one's own model, and the builder sums the two
    /// into the card's displayed counts.
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Every Sub-agent spawned under this session, running and finished alike, in
    /// spawn order — the card's badge and roster, and the *only* record of fan-out
    /// spend. A parallel set of sub-token counters used to run beside it; that
    /// arrangement is what let Claude's Sub-agent accounting reach the bin unseen,
    /// so the totals are derived from the roster instead (ADR 0014).
    pub sub_agent_roster: Vec<SubAgent>,
    pub activity: Option<String>,
    /// Latest entry timestamp; falls back to the file mtime when a source records
    /// no timestamps.
    pub last_event_at: Option<DateTime<Utc>>,
    /// The current structured need the source's [`AttentionReducer`] holds, or
    /// `None` if nothing does. Attention Since is still `Option` here — the shared
    /// builder resolves it against the file mtime. This is the only Attention input
    /// the shared status rule reads (via `is_some`).
    ///
    /// [`AttentionReducer`]: crate::attention::AttentionReducer
    pub attention: Option<PendingAttention>,
}

/// The shared status rule. Attention outranks everything: a present attention
/// reason wins regardless of the quiet window *and* of process death (an
/// unanswered wait must reach a human even after its process is gone — the card
/// says the process exited rather than silently filing under Finished).
/// Otherwise process liveness is ground truth where we have it: an alive agent
/// is Active no matter how quiet its transcript, a (debounced) dead one is
/// Finished no matter how fresh. Only without liveness data does the mtime
/// window decide. Staleness alone never produces Attention. Identical for every
/// source.
///
/// `has_active_sub_agents` is the one refinement of the staleness fallback: a parent
/// still fanning work out to Sub-agents is *working*, never stale, so it stays Active
/// even if its own main-loop transcript has gone quiet past the window. It does not
/// override a process-liveness verdict (a dead process is ground truth either way).
pub fn status_for(
    has_attention: bool,
    has_active_sub_agents: bool,
    mtime: DateTime<Utc>,
    now: DateTime<Utc>,
    liveness: ProcessLiveness,
) -> Status {
    if has_attention {
        Status::Attention
    } else {
        match liveness {
            ProcessLiveness::Alive => Status::Active,
            ProcessLiveness::Dead => Status::Finished,
            ProcessLiveness::Unknown => {
                if has_active_sub_agents {
                    Status::Active
                } else if now.signed_duration_since(mtime) >= ACTIVITY_WINDOW {
                    Status::Finished
                } else {
                    Status::Active
                }
            }
        }
    }
}

/// Turn a source's [`Projection`] into the UI [`Session`] — the shared,
/// source-agnostic assembly every [`Fold`] crosses on its way to a card.
///
/// Pure: given a projection and the two clocks plus the process verdict, it derives
/// the status (via [`status_for`]), resolves the atomic Attention value, and sums
/// cost and tokens — no I/O, no `self`. This is the test surface for that lifecycle
/// policy; construct a [`Projection`] and assert on the returned [`Session`] rather
/// than driving raw transcript bytes through a fold.
///
/// The rules it owns:
/// - **Attention is resolved only when `status == Attention`**, so the atomic
///   `attention` value and the status column can never disagree on the wire.
/// - **An unanswered wait survives process death**: the wait still needs a human,
///   but a local, factual `· process exited` note rides after the source-faithful
///   evidence so the card does not pretend the session is resumable in place.
/// - **Attention Since falls back to the file mtime** when the source recorded no
///   timestamp for the need.
/// - **Cost is a pure sum**: the session's own usage is priced at its model; each
///   Sub-agent's was already priced at that Sub-agent's own model as it was folded
///   (they may run cheaper ones), so the two are added here. When the session's model
///   is unpriced but Sub-agents ran priced models, the card still shows their cost
///   rather than nothing.
/// - **Token counts include Sub-agent usage** — fan-out spend is real and counted.
/// - **A Sub-agent is never Running when its root is not.** The roster's raw Running
///   flags feed the status rule first; only if the result is Finished is a row still
///   marked Running presented as Finished, with no outcome word. Stated in that
///   order the rule is well-founded, and it can only fire when Process Liveness says
///   the process is dead — exactly the gap it exists to close, where a session died
///   before its Sub-agent's completion arrived (6 of 59 observed spawns).
///
/// The live git `diff` is out-of-transcript, and `machine` is stamped at the source,
/// so both are left `None` here — the board (or a Collector) fills them.
pub fn assemble(
    p: Projection,
    mtime: DateTime<Utc>,
    now: DateTime<Utc>,
    liveness: ProcessLiveness,
) -> Session {
    let status = status_for(
        p.attention.is_some(),
        p.sub_agent_roster
            .iter()
            .any(|s| s.state == SubAgentState::Running),
        mtime,
        now,
        liveness,
    );
    let attention = (status == Status::Attention)
        .then_some(p.attention)
        .flatten()
        .map(|a| Attention {
            cause: a.cause,
            evidence: match (a.evidence, liveness == ProcessLiveness::Dead) {
                (Some(e), true) => Some(format!("{e} · process exited")),
                (None, true) => Some("process exited".to_string()),
                (e, false) => e,
            },
            since: a.since.unwrap_or(mtime),
            details_on_source: false,
            remote_evidence: a.remote_evidence,
        });
    let mut sub_agent_roster = p.sub_agent_roster;
    // Parent-dominance, applied *after* the status above was computed from the raw
    // flags: a Sub-agent is never Running when its root is not.
    if status == Status::Finished {
        for entry in &mut sub_agent_roster {
            if entry.state == SubAgentState::Running {
                entry.state = SubAgentState::Finished;
                entry.outcome = None;
            }
        }
    }
    let sub_cost: f64 = sub_agent_roster.iter().filter_map(|s| s.cost_usd).sum();
    let main_cost =
        crate::pricing::estimate_cost_usd(p.model.as_deref(), p.tokens_in, p.tokens_out);
    let cost_usd = match main_cost {
        Some(main) => Some(main + sub_cost),
        None if sub_cost > 0.0 => Some(sub_cost),
        None => None,
    };
    let tokens_in = p.tokens_in + sub_agent_roster.iter().map(|s| s.tokens_in).sum::<u64>();
    let tokens_out = p.tokens_out + sub_agent_roster.iter().map(|s| s.tokens_out).sum::<u64>();
    Session {
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
        sub_agent_roster,
        machine: None,
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

/// Project name shown on a card, derived from a `cwd`. Shared.
///
/// Normally the last path segment. Conductor worktrees live at
/// `…/conductor/workspaces/<repo>/<workspace>`, where the last segment is an
/// auto-generated workspace name that says nothing about the repo — those
/// render as `<workspace> (<repo>)`.
pub(crate) fn project_from_cwd(cwd: &str) -> String {
    let mut segs = cwd.trim_end_matches('/').rsplit('/');
    let last = segs.next().unwrap_or(cwd);
    if let (Some(repo), Some("workspaces"), Some("conductor")) =
        (segs.next(), segs.next(), segs.next())
    {
        return format!("{last} ({repo})");
    }
    last.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AttentionCause;

    #[test]
    fn plain_cwd_uses_last_segment() {
        assert_eq!(project_from_cwd("/Users/x/repos/foo"), "foo");
        assert_eq!(project_from_cwd("/Users/x/repos/foo/"), "foo");
    }

    #[test]
    fn conductor_workspace_includes_repo_name() {
        assert_eq!(
            project_from_cwd("/Users/x/conductor/workspaces/riku/surat"),
            "surat (riku)"
        );
    }

    #[test]
    fn workspaces_dir_outside_conductor_is_untouched() {
        assert_eq!(project_from_cwd("/Users/x/workspaces/riku/surat"), "surat");
    }

    // --- assemble(): the shared projection → Session seam ---------------------
    //
    // These exercise the lifecycle policy directly over a `Projection` literal —
    // no fold, no transcript bytes. What each source *translates* into a
    // projection is a fold concern, tested per source (session.rs / codex.rs);
    // what a projection *becomes* on the card is this seam's, tested here.

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// A minimal projection: a Claude session with a priced model, no pending
    /// need, no Sub-agents. Tests tweak only the fields they exercise.
    fn projection() -> Projection {
        Projection {
            id: "s1".into(),
            tool: Tool::Claude,
            project: "foo".into(),
            model: Some("claude-opus-4-8".into()),
            branch: None,
            cwd: Some("/a/foo".into()),
            tokens_in: 0,
            tokens_out: 0,
            sub_agent_roster: Vec::new(),
            activity: None,
            last_event_at: Some(ts("2026-07-19T10:00:00Z")),
            attention: None,
        }
    }

    /// One roster entry: a Running Sub-agent with its own spend, priced at its own
    /// model. Tests tweak only the fields they exercise.
    fn sub_agent(id: &str, model: &str, tin: u64, tout: u64) -> SubAgent {
        SubAgent {
            id: id.into(),
            spawn_key: format!("toolu_{id}"),
            errand: Some(format!("errand for {id}")),
            state: SubAgentState::Running,
            outcome: None,
            tokens_in: tin,
            tokens_out: tout,
            cost_usd: crate::pricing::estimate_cost_usd(Some(model), tin, tout),
            model: Some(model.into()),
            depth: 1,
            last_event_at: Some(ts("2026-07-19T10:00:05Z")),
        }
    }

    fn waiting(since: Option<DateTime<Utc>>, evidence: Option<&str>) -> PendingAttention {
        PendingAttention {
            cause: AttentionCause::Input,
            since,
            evidence: evidence.map(str::to_string),
            remote_evidence: None,
        }
    }

    #[test]
    fn alive_process_is_active_despite_a_stale_mtime() {
        // Process liveness is ground truth: a live agent quiet past the window is
        // still Active.
        let s = assemble(
            projection(),
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:25:00Z"),
            ProcessLiveness::Alive,
        );
        assert_eq!(s.status, Status::Active);
    }

    #[test]
    fn dead_process_is_finished_despite_a_fresh_mtime() {
        // The Ctrl-C false positive: file fresh, process gone → Finished.
        let s = assemble(
            projection(),
            ts("2026-07-19T10:04:00Z"),
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Dead,
        );
        assert_eq!(s.status, Status::Finished);
    }

    #[test]
    fn a_pending_need_outranks_a_stale_mtime() {
        // Attention outranks staleness: a present need wins even 30 min quiet.
        let mut p = projection();
        p.attention = Some(waiting(Some(ts("2026-07-19T10:00:00Z")), None));
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:30:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.status, Status::Attention);
        assert_eq!(s.attention.unwrap().cause, AttentionCause::Input);
    }

    #[test]
    fn a_quiet_session_without_liveness_is_finished() {
        // Without a liveness verdict the mtime window decides: 20 min quiet → Finished.
        let s = assemble(
            projection(),
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:20:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.status, Status::Finished);
    }

    #[test]
    fn attention_survives_process_death_and_annotates_evidence() {
        // An unanswered wait still needs a human after Ctrl-C; the card keeps the
        // source evidence and appends a factual "process exited" note.
        let mut p = projection();
        p.attention = Some(waiting(
            Some(ts("2026-07-19T10:00:00Z")),
            Some("Bash: cargo test"),
        ));
        let s = assemble(
            p,
            ts("2026-07-19T10:04:00Z"),
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Dead,
        );
        assert_eq!(s.status, Status::Attention);
        assert_eq!(
            s.attention.unwrap().evidence.as_deref(),
            Some("Bash: cargo test · process exited")
        );
    }

    #[test]
    fn process_death_note_stands_alone_when_there_is_no_evidence() {
        // No source excerpt but the process is gone: the note is the whole evidence.
        let mut p = projection();
        p.attention = Some(waiting(Some(ts("2026-07-19T10:00:00Z")), None));
        let s = assemble(
            p,
            ts("2026-07-19T10:04:00Z"),
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Dead,
        );
        assert_eq!(
            s.attention.unwrap().evidence.as_deref(),
            Some("process exited")
        );
    }

    #[test]
    fn attention_since_falls_back_to_mtime_when_the_need_has_none() {
        // A source that recorded no timestamp for the need: Since is stamped from
        // the file mtime rather than left absent.
        let mut p = projection();
        p.attention = Some(waiting(None, Some("Bash")));
        let mtime = ts("2026-07-19T10:04:00Z");
        let s = assemble(
            p,
            mtime,
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.attention.unwrap().since, mtime);
    }

    #[test]
    fn sub_agent_cost_surfaces_when_the_main_model_is_unpriced() {
        // An unknown orchestrator model has no main cost, but a priced Sub-agent's
        // spend still surfaces rather than vanishing.
        let mut p = projection();
        p.model = Some("some-future-model".into());
        p.sub_agent_roster = vec![sub_agent("a1", "claude-haiku-4-5", 1_000_000, 0)];
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Unknown,
        );
        let cost = s.cost_usd.expect("Sub-agent cost surfaces");
        assert!((cost - 0.80).abs() < 1e-9, "cost was {cost}");
    }

    #[test]
    fn card_tokens_and_cost_are_the_roster_summed_at_each_own_model() {
        // Fan-out spend is real and counted, and each Sub-agent is priced at the model
        // *it* ran — an Opus orchestrator with a Haiku child costs 90 + 0.80, never
        // the 105 an Opus-priced child would give.
        let mut p = projection();
        p.tokens_in = 1_000_000;
        p.tokens_out = 1_000_000;
        p.sub_agent_roster = vec![sub_agent("a1", "claude-haiku-4-5", 1_000_000, 0)];
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.tokens_in, 2_000_000);
        assert_eq!(s.tokens_out, 1_000_000);
        let cost = s.cost_usd.unwrap();
        assert!((cost - 90.80).abs() < 1e-9, "cost was {cost}");
        // The card's model stays the orchestrator's, never a Sub-agent's.
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn a_running_sub_agent_keeps_a_quiet_parent_working() {
        // The Staleness refinement, with a truthful input at last: the parent's own
        // transcript has been quiet for 25 minutes — well past the 15-minute window,
        // and inside the 963s of observed fan-out silence — but it is working.
        let mut p = projection();
        p.sub_agent_roster = vec![sub_agent("a1", "claude-haiku-4-5", 500, 50)];
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:25:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.status, Status::Active);
        assert_eq!(s.sub_agent_roster[0].state, SubAgentState::Running);
    }

    #[test]
    fn a_dead_process_finishes_the_parent_and_its_running_sub_agents_with_it() {
        // Parent-dominance: a Sub-agent is never Running when its root is not. The
        // refinement above does not override a Process Liveness verdict, and the row
        // that was Running is presented Finished — with no outcome word, since the
        // source never said one.
        let mut p = projection();
        p.sub_agent_roster = vec![sub_agent("a1", "claude-haiku-4-5", 500, 50)];
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:00:30Z"),
            ProcessLiveness::Dead,
        );
        assert_eq!(s.status, Status::Finished);
        assert_eq!(s.sub_agent_roster[0].state, SubAgentState::Finished);
        assert_eq!(s.sub_agent_roster[0].outcome, None);
        // Demotion is presentation, not erasure: its spend still counts.
        assert_eq!(s.tokens_in, 500);
    }

    #[test]
    fn a_running_sub_agent_never_makes_the_parent_wait_on_a_human() {
        // Fanning out is not a human wait: with no pending need, a parent full of
        // Running Sub-agents is Active, never Attention.
        let mut p = projection();
        p.sub_agent_roster = vec![
            sub_agent("a1", "claude-haiku-4-5", 10, 1),
            sub_agent("a2", "claude-haiku-4-5", 10, 1),
        ];
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:00:30Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.status, Status::Active);
        assert!(s.attention.is_none());
    }

    // --- merge_roster(): the union of the parent's spawns and its children's files -

    #[test]
    fn a_spawn_and_its_child_file_are_one_row() {
        // The parent said a Sub-agent exists and what it was for; the child file says
        // what it spent. Keyed by the spawn's tool-use id, that is one row.
        let spawn = SubAgent {
            spawn_key: "toolu_a".into(),
            errand: Some("map the parser".into()),
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            model: None,
            depth: 0,
            last_event_at: None,
            ..sub_agent("toolu_a", "claude-haiku-4-5", 0, 0)
        };
        let child = SubAgent {
            spawn_key: "toolu_a".into(),
            errand: None,
            ..sub_agent("a1b2c3", "claude-haiku-4-5", 900, 90)
        };
        let roster = merge_roster(vec![spawn], vec![child]);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].id, "a1b2c3");
        assert_eq!(roster[0].errand.as_deref(), Some("map the parser"));
        assert_eq!(roster[0].tokens_in, 900);
        assert_eq!(roster[0].depth, 1);
    }

    #[test]
    fn the_side_that_states_an_outcome_decides_how_a_sub_agent_ended() {
        // The two sides of a row say different things, and how it ended is one of
        // them. For Claude that is the parent's notification — the child's own file
        // states no outcome and is still being written when it arrives, so a fresh
        // fold of it must not put a finished Sub-agent back to Running.
        let spawn = SubAgent {
            spawn_key: "toolu_a".into(),
            state: SubAgentState::Finished,
            outcome: Some("failed".into()),
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            ..sub_agent("toolu_a", "claude-haiku-4-5", 0, 0)
        };
        let child = SubAgent {
            spawn_key: "toolu_a".into(),
            ..sub_agent("a1b2c3", "claude-haiku-4-5", 900, 90)
        };
        let roster = merge_roster(vec![spawn], vec![child.clone()]);
        assert_eq!(roster[0].state, SubAgentState::Finished);
        assert_eq!(roster[0].outcome.as_deref(), Some("failed"));
        assert_eq!(roster[0].tokens_in, 900, "and still what the child spent");

        // The other way round is the Codex shape, where the terminal marker is in the
        // child's own rollout and the parent recorded no notification at all.
        let running_spawn = SubAgent {
            spawn_key: "toolu_a".into(),
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            ..sub_agent("toolu_a", "claude-haiku-4-5", 0, 0)
        };
        let finished_child = SubAgent {
            state: SubAgentState::Finished,
            outcome: Some("completed".into()),
            ..child
        };
        let roster = merge_roster(vec![running_spawn], vec![finished_child]);
        assert_eq!(roster[0].state, SubAgentState::Finished);
        assert_eq!(roster[0].outcome.as_deref(), Some("completed"));
    }

    #[test]
    fn either_side_alone_is_a_row() {
        // A spawn whose child file has not appeared yet is a row that says what it was
        // sent to do; a child whose spawn was recorded in *another* child's transcript
        // — the depth-2 case — is a row that says what it spent. Spawn order first,
        // then what only the children knew about.
        let spawn = SubAgent {
            spawn_key: "toolu_a".into(),
            errand: Some("still starting".into()),
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            ..sub_agent("toolu_a", "claude-haiku-4-5", 0, 0)
        };
        let nested = SubAgent {
            spawn_key: "toolu_deep".into(),
            depth: 2,
            ..sub_agent("a-deep", "claude-haiku-4-5", 100, 10)
        };
        let roster = merge_roster(vec![spawn], vec![nested]);
        assert_eq!(
            roster.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["toolu_a", "a-deep"]
        );
        assert_eq!(roster[1].depth, 2);
    }

    #[test]
    fn a_fan_out_parent_stays_running_until_a_completion_says_otherwise() {
        // A Running row keeps its parent out of Finished however long its own
        // transcript has been quiet — which is right while a Sub-agent is genuinely
        // running, and was the whole board's behaviour for a session that ever fanned
        // out while nothing read completions.
        let mut p = projection();
        p.sub_agent_roster = vec![sub_agent("a1", "claude-haiku-4-5", 500, 50)];
        let s = assemble(
            p.clone(),
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T18:00:00Z"), // eight hours quiet
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.status, Status::Active);
        assert_eq!(s.sub_agent_roster[0].state, SubAgentState::Running);

        // A completion is what releases it: with the fan-out over, the same quiet
        // parent ages into Finished on the Staleness rule like any other session, and
        // the word the source stated survives that.
        let mut done = p.clone();
        done.sub_agent_roster[0].state = SubAgentState::Finished;
        done.sub_agent_roster[0].outcome = Some("completed".into());
        let s = assemble(
            done,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T18:00:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.status, Status::Finished);
        assert_eq!(s.sub_agent_roster[0].outcome.as_deref(), Some("completed"));

        // And the other thing that finishes it, which is what keeps the first
        // assertion honest when no completion ever arrives.
        let dead = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T18:00:00Z"),
            ProcessLiveness::Dead,
        );
        assert_eq!(dead.status, Status::Finished);
        assert_eq!(dead.sub_agent_roster[0].state, SubAgentState::Finished);
    }

    #[test]
    fn two_contributions_cannot_honestly_share_a_spawn_key() {
        // Claude mints one spawn key per `Agent` tool-use and the sidecar carries it
        // back, so a collision means a file is lying about which spawn it came from.
        // The later row wins and the earlier one's spend leaves the card — pinned
        // here because the totals are derived from this roster, so a dropped row is a
        // dropped number rather than a cosmetic loss.
        let spawn = SubAgent {
            spawn_key: "toolu_a".into(),
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            ..sub_agent("toolu_a", "claude-haiku-4-5", 0, 0)
        };
        let first = SubAgent {
            spawn_key: "toolu_a".into(),
            ..sub_agent("a1", "claude-haiku-4-5", 900, 90)
        };
        let second = SubAgent {
            spawn_key: "toolu_a".into(),
            ..sub_agent("a2", "claude-haiku-4-5", 100, 10)
        };
        let roster = merge_roster(vec![spawn], vec![first, second]);
        assert_eq!(roster.len(), 1, "one spawn, one row");
        assert_eq!(roster[0].id, "a2", "the later contribution takes the row");
    }

    #[test]
    fn child_only_rows_land_in_a_stable_order() {
        // The store's file map has no order of its own, so rows it alone contributes
        // are ordered oldest-first — otherwise an unchanged roster would compare
        // unequal and the board would churn upserts.
        let older = SubAgent {
            last_event_at: Some(ts("2026-07-19T10:00:01Z")),
            ..sub_agent("z-first", "claude-haiku-4-5", 1, 1)
        };
        let newer = SubAgent {
            last_event_at: Some(ts("2026-07-19T10:00:09Z")),
            ..sub_agent("a-second", "claude-haiku-4-5", 1, 1)
        };
        let ids = |r: Vec<SubAgent>| r.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(
            ids(merge_roster(Vec::new(), vec![newer.clone(), older.clone()])),
            vec!["z-first", "a-second"]
        );
        assert_eq!(
            ids(merge_roster(Vec::new(), vec![older, newer])),
            vec!["z-first", "a-second"]
        );
    }
}
