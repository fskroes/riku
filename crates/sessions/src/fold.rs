//! The Session Source seam. Each tool (Claude Code, Codex CLI) decodes its own
//! transcript lines by implementing [`Fold`]; everything downstream — byte-offset
//! tailing, truncation reset, the mtime-based status heuristic, the [`Session`]
//! shape — is shared and lives in `session.rs`.
//!
//! A [`Fold`] folds a file's lines into a [`Projection`]: the tool-agnostic inputs
//! the shared builder needs. The status column is derived here from
//! [`Projection::attention`] plus file mtime, so no source reimplements it.

use chrono::{DateTime, Duration, Utc};

use crate::attention::PendingAttention;
use crate::liveness::ProcessLiveness;
use crate::model::{Attention, Session, Status, SubAgents, Tool};

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
///
/// `PartialEq` only (not `Eq`): `sub_agent_cost_usd` is an `f64`.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub id: String,
    pub tool: Tool,
    pub project: String,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    /// Main-conversation assistant token usage. The Sub-agent (sidechain) usage is
    /// kept apart in `sub_tokens_*` so cost can be priced per each agent's own model;
    /// the builder sums the two into the card's displayed counts.
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Sub-agent (sidechain) assistant token usage folded into this parent, held
    /// separately only for per-model pricing (see `sub_agent_cost_usd`). `0` for a
    /// source with no Sub-agent concept.
    pub sub_tokens_in: u64,
    pub sub_tokens_out: u64,
    /// Cost of the Sub-agent usage, already priced per each Sub-agent entry's *own*
    /// model (they may run cheaper models than the orchestrator). The builder adds it
    /// to the main-model cost. `0.0` when there is no Sub-agent usage.
    pub sub_agent_cost_usd: f64,
    /// The Sub-agents currently fanning out under this session — the card's badge.
    /// Empty for a source with no Sub-agent concept.
    pub sub_agents: SubAgents,
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
/// - **Cost is a pure sum**: the main-conversation usage is priced at the session
///   model; the Sub-agent usage was already priced per each Sub-agent's own model as
///   it was folded (they may run cheaper models), so the two are added here. When the
///   main model is unpriced but Sub-agents ran priced models, the card still shows
///   their cost rather than nothing.
/// - **Token counts include Sub-agent usage** — fan-out spend is real and counted
///   (the split exists only so cost could be priced per model).
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
        p.sub_agents.active > 0,
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
    let main_cost = crate::pricing::estimate_cost_usd(p.model.as_deref(), p.tokens_in, p.tokens_out);
    let cost_usd = match main_cost {
        Some(main) => Some(main + p.sub_agent_cost_usd),
        None if p.sub_agent_cost_usd > 0.0 => Some(p.sub_agent_cost_usd),
        None => None,
    };
    let tokens_in = p.tokens_in + p.sub_tokens_in;
    let tokens_out = p.tokens_out + p.sub_tokens_out;
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
        sub_agents: p.sub_agents,
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
            sub_tokens_in: 0,
            sub_tokens_out: 0,
            sub_agent_cost_usd: 0.0,
            sub_agents: SubAgents::default(),
            activity: None,
            last_event_at: Some(ts("2026-07-19T10:00:00Z")),
            attention: None,
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
        assert_eq!(s.attention.unwrap().evidence.as_deref(), Some("process exited"));
    }

    #[test]
    fn attention_since_falls_back_to_mtime_when_the_need_has_none() {
        // A source that recorded no timestamp for the need: Since is stamped from
        // the file mtime rather than left absent.
        let mut p = projection();
        p.attention = Some(waiting(None, Some("Bash")));
        let mtime = ts("2026-07-19T10:04:00Z");
        let s = assemble(p, mtime, ts("2026-07-19T10:05:00Z"), ProcessLiveness::Unknown);
        assert_eq!(s.attention.unwrap().since, mtime);
    }

    #[test]
    fn sub_agent_cost_surfaces_when_the_main_model_is_unpriced() {
        // An unknown orchestrator model has no main cost, but a priced Sub-agent's
        // spend still surfaces rather than vanishing.
        let mut p = projection();
        p.model = Some("some-future-model".into());
        p.sub_agent_cost_usd = 0.80;
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
    fn card_tokens_include_sub_agent_usage() {
        // The split of main vs Sub-agent tokens exists only for per-model pricing;
        // the card's counts are the sum.
        let mut p = projection();
        p.tokens_in = 100;
        p.tokens_out = 10;
        p.sub_tokens_in = 40;
        p.sub_tokens_out = 4;
        let s = assemble(
            p,
            ts("2026-07-19T10:00:00Z"),
            ts("2026-07-19T10:05:00Z"),
            ProcessLiveness::Unknown,
        );
        assert_eq!(s.tokens_in, 140);
        assert_eq!(s.tokens_out, 14);
    }
}
