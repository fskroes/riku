# Agent Board

A mission-control dashboard that monitors AI coding agent sessions — the user's own local sessions and those of teammates on remote machines — and displays them as cards on a status board, linked to the Work Items they are carrying out.

## Language

**Agent Session**:
One run of a coding agent (e.g. a Claude Code or Codex CLI conversation) working in a repo on a machine. The unit displayed as a card on the board.
_Avoid_: task, job, run, workspace

**Attention**:
The board status for an Agent Session that reliably has one current human need: waiting on input (approval, question, or review) or ended in error. Attention belongs to the Agent Session, not necessarily to the person viewing a shared board; a newer structured need replaces the current one, and soft signals like staleness are card hints, never an Attention move.
_Avoid_: blocked, stuck, warning

**Attention Cause**:
The structured kind of human response an Agent Session requires: Approval required, Answer required, Review required, Session error, or the nonspecific fallback Input required. A specific cause comes only from structured source evidence, never inference from prose.
_Avoid_: reason, status message, guess

**Attention Evidence**:
A bounded, source-faithful excerpt from the observation that caused a local Agent Session to enter Attention, sanitized to remove recognized sensitive values. It supports the Attention cause without summarising or interpreting what the agent wants; when no safe excerpt can be extracted, it is absent.
_Avoid_: summary, explanation, rationale

**Remote Attention Evidence**:
A bounded rendering of allowlisted structured fields from the observation that caused a remote Agent Session to enter Attention. It excludes arbitrary commands, arguments, prose, and error output; when the allowlisted fields cannot explain the need, full details remain available only on the source machine.
_Avoid_: redacted transcript, remote excerpt, summary

**Attention Since**:
The time the current human need began. It determines the oldest-waiting-first order; it changes when a newer structured need replaces the current one, but not for observations that leave the need unchanged.
_Avoid_: last activity, session age, priority

**Attention Resolution**:
Structured source evidence that the current human need was answered, cancelled, withdrawn, or superseded by resumed or completed work. Generic activity and elapsed time are not Attention Resolution.
_Avoid_: forward progress, activity, acknowledgement

**Collector**:
A small process running on a machine that watches that machine's Agent Sessions (via Session Sources) and pushes updates to the Relay.
_Avoid_: daemon, agent (clashes with AI agent), watcher

**Relay**:
The lightweight hosted service that receives Collector updates from remote machines and streams them to boards. Not required for solo/local use.
_Avoid_: server, backend, hub

**Work Item**:
A unit of project work to be done (a GitHub Issue or an entry in a Work Map file). An Agent Session can be linked to the Work Item it is working on.
_Avoid_: task, ticket, todo, issue (except when specifically a GitHub Issue)

**Work Link**:
An ephemeral association between a Work Item and the most recently active local Agent Session whose git branch contains that item's id. It is inferred from the current branch name only; it is neither stored nor manually editable. A Work Link outlives its session's activity — it survives the session going Finished — so its existence alone never means the item is being worked; only a **live** Work Link (see Work Item Status) does.
_Avoid_: assignment, mapping

**Work Item Status**:
Which of To do / In progress / Done the board shows for a Work Item. A source can only assert To do or Done on its own — In progress exists there just as a hand-written Work Map marker (`[~]`, `[-]`, `[/]`) or a GitHub `in-progress`/`doing` label — so a **live** Work Link, one whose Agent Session is Running or in Attention, raises an unmarked item to In progress from evidence instead (#66). The source's own word travels beside it and the card discloses any difference, so the plan is never silently overwritten; Done is never overridden, since work continuing on a branch must not un-complete an item.
_Avoid_: lane, state, progress

**Work Map**:
A Markdown checklist (`WORK.md`) listing a project's Work Items, each with a short stable id (`W-14`). A project's single source of Work Items: if `WORK.md` exists it wins; otherwise GitHub Issues is used. Never both.
_Avoid_: task list, backlog file, plan

**Process Liveness**:
The observed state of an Agent Session's actual agent process — Alive, Dead (after a two-miss debounce), or Unknown — matched by working directory once per refresh tick. Where a verdict exists it is ground truth for Running vs Finished in both directions; Staleness applies only when the verdict is Unknown, and Attention outranks even a dead process (ADR 0011).
_Avoid_: heuristic, health check, activity

**Staleness**:
The fallback mtime rule: a transcript quiet past the 15-minute activity window counts as Finished. A heuristic, not ground truth — it decides status only for Agent Sessions without a Process Liveness verdict.
_Avoid_: finished (as a synonym), timeout, expiry

**Handoff Status**:
The agent's parting assessment, written in its journal entry at session stop, of where the effort stands: needs-you, needs-review, or on-track. Distinct from Attention — Attention is a live, source-evidence-only status of a running Agent Session, while Handoff Status is a judgment recorded at the moment the session ends. It orders journal cards (needs-you → needs-review → on-track) and, being interpretation, is corrected by a user journal entry, latest-wins.
_Avoid_: blocked, status (bare), attention (for stopped sessions)

**Project Journal**:
An append-only, opt-in log of one project's handoffs, in prose: the coding agent appends an entry as its session stops, and the user answers with correction entries of their own. Riku reads and renders it but never writes it on its own behalf, and it annotates the derived timeline rather than replacing it. Both voices are equal — the latest entry wins for Handoff Status and next step whoever wrote it. One file per project, named by a stable slug of the project's directory path: the filename contract the writing hook and Riku both hold to (ADR 0013).
_Avoid_: log, history, notes, agent summary

**Recap**:
The board's reading of the Project Journal: one card per thread of effort — a project's journal and the Agent Sessions behind it — ordered by Handoff Status, carrying what the authors say was done (grouped by day), the next step, the entry's age, and the resume command Riku builds itself. The derived transcript timeline stays beside the prose, since the journal annotates ground truth rather than replacing it; the day view is a lens over the same cards, not the home. All journal prose is rendered as text and never executed (ADR 0013).
_Avoid_: activity feed, digest, standup, summary view

**Session Source**:
An adapter that discovers and reads Agent Sessions for one agent tool (Claude Code, Codex CLI). Each supported tool has exactly one Session Source.
_Avoid_: provider, integration, connector

**Sub-agent**:
Work an Agent Session fans out to a child agent (a Claude Code `Task` — transcript-marked `isSidechain`). A Sub-agent is never its own card: it surfaces as a badge on the parent's card counting the currently-active ones, with each active one's short description in the badge tooltip. Its token usage and cost fold into the parent (cost priced per the Sub-agent's own model, since it may run a cheaper one); a Sub-agent event keeps the parent looking alive. Codex CLI has no comparable concept, so its cards carry none.
_Avoid_: sidechain (transcript jargon only), subtask, child session
