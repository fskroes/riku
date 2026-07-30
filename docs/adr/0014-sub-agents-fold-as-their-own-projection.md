# 0014 — Sub-agents fold as their own projection, never as sessions

## Status

Accepted (2026-07-29).

## Context

The Sub-agent badge (issue #23) has never once rendered. Four independent
breakages, each invisible because the failure mode is silence.

(All figures below were measured on 2026-07-29 against this machine's
transcripts. It is a live corpus, not a fixture — the Claude sub-agent count grew
from 60 to 64 during the investigation. Counts are a snapshot; the ratios and
zeros held throughout.)

- `session.rs` keyed the badge on a `Task` tool-use. The tool was renamed:
  across the whole corpus, **0** transcripts contain a `Task` tool-use and
  **30** parent transcripts contain `Agent`.
- Sub-agent turns left the parent transcript. They now live in
  `<project>/<parent-uuid>/subagents/agent-<agentId>.jsonl` beside a
  `.meta.json` sidecar. Parent-side `isSidechain` entries: **0**. So the fold's
  sidechain branch never fires and `sub_agent_tokens` / `sub_agent_cost_usd`
  stay 0.
- Those child files *are* discovered (`MAX_SCAN_DEPTH = 6` reaches them,
  `ClaudeSource::owns` accepts them) and folded — but the sidechain early-return
  runs *before* `self.id` is assigned, so `projection()` returns `None` and every
  fold is discarded. Every Claude sub-agent ever recorded on this machine — 60 at
  the time of measurement, ~66M input tokens — binned.
- **The retire rule is wrong at the concept level.** A Sub-agent's `tool_result`
  is a launch acknowledgement — *"Async agent launched successfully… you will be
  notified automatically when it completes"* — arriving ~2s after the spawn.
  Completion arrives as a separate `<task-notification>` carrying
  `<tool-use-id>` and a `<status>`, up to 20 minutes later. Retiring on
  `tool_result` would zero the badge ~2s after every spawn, so fixing the rename
  alone would not have fixed the badge.

Codex was excluded on a premise that is simply false. It has **75** sub-agent
rollouts carrying a *richer* model than Claude's: `parent_thread_id` (resolves
75/75), `depth`, `agent_path`, `agent_nickname`, `agent_role`, and a
`task_complete` event (74/75).

So the two tools are mirror images. Claude gives the **root** free (directory
name, and `sessionId` on every child entry is the root's) and charges a
cross-file `toolUseId` join for the immediate spawner. Codex gives the
**immediate spawner** free and charges a walk up the chain for the root.

## Decision

A Sub-agent is **folded in full but never promoted to a card**, and it is a
**distinct projection type** — never a `Session` carrying a parent field.

- **Never a card, and now with a reason.** A Sub-agent carries no independent
  human need: it cannot be approved, answered, or resumed by a person, only by
  the Agent Session that sent it. That reason is tool-agnostic, which is why the
  same rule now governs Codex.
- **"Not a card" stops meaning "not folded."** Those were the same thing by
  accident, and the accident is what binned the tokens.
- **Its own type, not `Session` + `parent`.** See Trade-offs — the reuse option
  is actively harmful, not merely inelegant.
- **The parent carries all of its Sub-agents**, running or finished; `active`
  becomes a property of each rather than of the collection. Carrying only the
  active set means every completed session shows nothing, since a completed
  session by definition has none.
- **Attachment is to the root Agent Session** — the only node that is a card.
  Depth is recorded (Claude `spawnDepth`, Codex `depth`) but not drawn: 5 of 135
  observed sub-agents nest at all, max depth 3.
- **State is Running / Finished with a verbatim outcome** (`completed`,
  `failed`, `stopped`, `killed`), read from the notification's `<status>` tag,
  never inferred from prose. **Latest notification wins** — 97 of 101 task-ids
  notify more than once, because a Sub-agent can be resumed after finishing. One
  dominating rule closes the gap where a session dies before its notification
  arrives (6 of 59 spawns): a Sub-agent is never Running when its root isn't,
  reusing ADR 0011's verdict rather than inventing a second liveness probe.
- **A failed Sub-agent never moves the parent to Attention.** The notification
  is addressed to the orchestrator, which reads it and continues — in 23 of 24
  observed non-`completed` notifications the parent's next entry came a median
  of 0.7s later. Promoting it would infer a human need from an agent-level
  event, which ADR 0010 rules out.
- **The full roster crosses the Relay.** Errand is the orchestrator's own
  one-line summary of what it delegated, structurally the same as `activity`
  (which already crosses unreduced) rather than an Attention Evidence source
  excerpt (which is bounded and reduced by design).
- **The parent's headline cost is the total**, with the roster's per-child
  tokens as the disclosure.

## Considered options

**A `Session` with `parent: Option<…>`** — one type, one pipeline, free reuse of
the attention reducer, status rule, pricing, and diff. Rejected because two
consumers written before Sub-agents existed already get it wrong:

- **Work Link would retarget.** `link_session` picks `max_by_key(last_event_at)`
  over every candidate whose branch matches. A Sub-agent inherits the parent's
  `gitBranch` verbatim and is usually *more* recently active. The Work Item chip
  would point at a Sub-agent — a card that does not exist.
- **Process Liveness would collapse.** Per ADR 0011, only one session per
  directory takes the liveness credit. A Sub-agent shares the parent's cwd
  exactly, so the *parent* would drop to `Unknown` and fall back to the staleness
  heuristic — precisely for the sessions doing the most work.

A distinct type makes the distinction unmissable at the compile boundary. The
codebase already voted this way one layer down: `parse_lsof_cwds` strips
`.claude/worktrees/agent-*` so Sub-agent *processes* never enter the liveness
pool.

**Rendering a foldable child block under the parent's row** (the Yaplog pattern
from the #65 research). Rejected because `CONTEXT.md`'s **Band** is *"a
labelled, counted run of rows… the count says how much is there"*. Expanding
seven child rows inside a Band makes its count stop describing what is on
screen — the exact failure #64/#65 established the `5 of 12` discipline to
prevent. The roster renders in the badge's existing panel instead.

**Filling every roster row with a per-tool fallback chain.** Rejected: Claude's
`description` covers 62/62, but Codex's only fully-covered field is
`agent_nickname` (`Dirac`, `Euclid`) which names nothing. Following Attention
Evidence — *"when no safe excerpt can be extracted, it is absent"* — an Errand is
present when the source states one and absent otherwise. A label that merely
looks like content is worse than a blank.

## Consequences

- **Codex card costs roughly double.** Measured against parents that spawned
  children, Codex sub-agents total **106%** of their parent's input tokens
  (worst case 3.61×, from 8 children); Claude's add **11%** (worst case 0.55×).
  This is the number becoming true for the first time, not an inflation — but it
  will read as a regression to anyone who does not know why, and belongs in a
  release note.
- **The wire migration is free.** Reshaping `SubAgents` normally raises "what
  about legacy Collector data?" — but the `Task` bug means every legacy
  Collector has only ever sent `{active: 0, descriptions: []}`. The existing
  `#[serde(default)]` degradation is sufficient; no `legacy_*` capture field of
  the kind `wire.rs` carries for Attention is needed.
- **`has_active_sub_agents` gains a truthful input.** The parent transcript
  genuinely goes quiet during fan-out — two observed spans exceed the 15-minute
  staleness window (max 963s). The refinement in `fold.rs` was written for
  exactly this and has never once fired.
- **Unit tests could not have caught any of this.** The fixtures hand-write
  `"name":"Task"` and inline `isSidechain` entries, asserting against a
  transcript shape Claude Code no longer produces. Fixtures for source formats
  we do not control need periodic re-grounding against real corpora.
- **Everything here was measured on transcripts at rest.** The Running state was
  inferred from spawn/notification pairs in completed sessions and has never
  been observed live. Verify against a session actively fanning out before
  treating the lifecycle as settled.

## Update — 2026-07-30, on building the Codex side (#76)

Three things this ADR states came out differently once the Codex join was built.
Recorded here rather than edited in above, so the decision keeps its date and the
correction keeps its own.

- **The outcome word is Riku's for Codex, not the source's.** Above, outcome is
  "read from the notification's `<status>` tag, never inferred from prose" — true
  of Claude, which names one of `completed` / `failed` / `stopped` / `killed`.
  Codex has no such field anywhere: it states an event *type*, `task_complete`,
  and nothing else. So `completed` is a mapping declared in one place in
  `codex.rs`, one terminal event to one word, rather than a token lifted out of a
  rollout. The rule that survives intact is the one that matters — a word is
  never *inferred*, and an ending Codex has no word for carries none.
- **An aborted turn ends a Codex Sub-agent too**, unworded. Not considered above,
  where `task_complete` on 74 of 75 rollouts made the remaining one look like a
  rounding error. It is not: a roster row that claims to be running is exactly
  what holds a quiet parent out of Finished, so one unterminated child pins its
  parent's card Active for as long as the parent lives, escapable only by a dead
  process. Ending it on the abort closes that, and a later `task_started` takes
  the ending back like any other resumption.
- **The ratios were re-measured and moved.** Above: Codex sub-agents total 106%
  of their parents' input tokens, over 75 rollouts. Against the corpus on
  2026-07-30 — 79 rollouts, 35 parents that spawned — it is **98%**. The worst
  case is unchanged at 3.61× from 8 children, and so is the conclusion the number
  was cited for: Codex card costs roughly double. This is a live corpus; the
  figure will keep moving, and the release note carries the measured one.

One thing this ADR did not anticipate at all. Codex rollouts **fork**, and a
forked thread replays the meta of the thread it forked from into its own history,
so a rollout can state several `session_meta` records. The fold took the last, and
the last is an ancestor's: 55 of 193 rollouts were running under their parent's
id. It never showed, because every one of them was a subagent rollout and those
were suppressed — the same "invisible because the failure mode is silence" this
ADR was written about, one layer down. The first `session_meta` is now the one
that identifies a rollout.

## Deliberately out of scope

A per-session detail surface (the roster's eventual right home — a hover panel
under-serves per-child tokens, cost, outcome, and Errand); drawing the nesting
tree (depth is carried, so adding it later is a rendering change plus a Claude-
side `toolUseId` index, never a re-fold); Codex's unread `sub_agent_activity`
and `inter_agent_communication_metadata` events; the per-task `output_file`.
