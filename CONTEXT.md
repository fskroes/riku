# Agent Board

A mission-control dashboard that monitors AI coding agent sessions — the user's own local sessions and those of teammates on remote machines — and displays them as cards on a status board, linked to the Work Items they are carrying out.

## Language

**Agent Session**:
One run of a coding agent (e.g. a Claude Code or Codex CLI conversation) working in a repo on a machine. The unit displayed as a card on the board.
_Avoid_: task, job, run, workspace

**Attention**:
The board status for an Agent Session that reliably needs a human: waiting on input (approval, question) or ended in error. Soft signals like staleness are card hints, never an Attention move.
_Avoid_: blocked, stuck, warning

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
An ephemeral association between a Work Item and the most recently active local Agent Session whose git branch contains that item's id. It is inferred from the current branch name only; it is neither stored nor manually editable.
_Avoid_: assignment, mapping

**Work Map**:
A Markdown checklist (`WORK.md`) listing a project's Work Items, each with a short stable id (`W-14`). A project's single source of Work Items: if `WORK.md` exists it wins; otherwise GitHub Issues is used. Never both.
_Avoid_: task list, backlog file, plan

**Session Source**:
An adapter that discovers and reads Agent Sessions for one agent tool (Claude Code, Codex CLI). Each supported tool has exactly one Session Source.
_Avoid_: provider, integration, connector
