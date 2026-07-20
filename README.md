# Agent Board

A local mission-control board for AI coding agent sessions. It watches your local
Claude Code **and** Codex CLI transcripts and renders every session live on an
attention-first board (issues #2, #5), with a per-project **Work Items** view that
links each session to the plan it is carrying out (issue #1, C4).

```
Cargo.toml            workspace
crates/collector/     lib — the Session Sources (Claude Code + Codex CLI) behind a
                      shared trait: discovery, transcript tailing, the Session
                      model, status heuristic
crates/board/         bin — axum server: serves web/dist + /api/sessions + SSE
web/                  React 18 + Vite 5 + TypeScript board (attention stream)
```

## Run it

```sh
# 1. Build the UI (once, and after web/ changes)
cd web && npm install && npm run build && cd ..

# 2. Run the board
cargo run -p board            # then open http://localhost:4242
```

Flags: `--port <n>` (default 4242, binds `127.0.0.1` only), `--root <dir>`
(Claude Code projects, default `~/.claude/projects`), `--codex-root <dir>` (Codex
CLI sessions, default `$CODEX_HOME/sessions` or `~/.codex/sessions`), `--web-dist
<dir>` (default `web/dist`). A missing Codex root degrades gracefully — Claude
sessions still show.

If `web/dist` is missing the server stays up and `/` returns a 503 telling you to
build the UI; the API keeps working.

## Develop the UI with hot reload

```sh
cargo run -p board            # API on :4242
cd web && npm run dev         # UI on :5173, proxies /api to :4242
```

## Test

```sh
cargo test                    # collector unit tests + board integration tests
cd web && npm run build       # type-checks the frontend (tsc, strict)
```

## How it works

Each transcript is one **Agent Session**. Two **Session Sources** plug in behind a
shared trait — Claude Code (`~/.claude/projects/<project>/<uuid>.jsonl`) and Codex
CLI (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`). Each source owns only its
discovery layout and line decoding; the byte-offset tailing (re-parsing on
truncation), the mtime-based status heuristic, and the `Session` shape are shared.

The collector scans transcripts touched in the last 24h and tails each
incrementally, folding lines into a `Session` while tolerating unknown fields /
schema drift. Subagent traffic never surfaces as its own card (Claude's
`isSidechain`; Codex's `thread_source: "subagent"`). Claude tokens are summed
per-entry; Codex tokens are the latest **cumulative** `total_token_usage`. Status
is mtime-based: an unanswered Claude `tool_use` in the newest entry is
**Attention**, a fresh file is **Active**, and a quiet one (≥15m) is **Finished**.
Codex's approval-wait signal is deferred to C3, so Codex cards are Active/Finished
only for now.

The board serves a full snapshot at `GET /api/sessions` and streams full-Session
updates over SSE at `GET /api/events` (`session` / `removed` events; the client
upserts by `id` and re-syncs the snapshot on reconnect). See
`docs/adr/0005-*` for the UI decision and `CONTEXT.md` for the domain language.

## Deep-link into a session (C6)

The board's one action on a card is to **deep-link into the local session** — never
to control it remotely (ADR 0002). Because the board runs on your own machine, an
Attention card's `Review →` (and every card's `open ↗`) opens a new terminal that
`cd`s into the session's workspace and resumes it — `claude --resume <id>` or `codex
resume <id>` — so you land back in the exact conversation to answer or review it.

`POST /api/sessions/:id/open` drives it. The only input is the session `id`; the
tool, working directory, and transcript are read from the store, so a request can
never point the launch at an arbitrary command or directory. The resume command is
run inside a fresh **Terminal.app** window (macOS); if a CLI names its resume flag
differently the terminal still opens in the right workspace. A launch failure is
surfaced on the card.

## Work Items (C4)

The **Work Items** tab shows one project's plan at a time, rendered two ways over
the same item set — a **To do / In progress / Done** kanban and a **dependency
graph** laid out by blocked-by depth. A project selector switches projects (drawn
from the live sessions), and a source badge shows whether the items came from
`WORK.md` or GitHub Issues.

Each project has a single **Work Source**, resolved by `GET /api/work?cwd=<dir>`:
if `<dir>/WORK.md` exists it wins, otherwise GitHub Issues via `gh` (degrading to
an empty list when `gh` is unavailable or the dir is not a repo). `WORK.md` is a
Markdown checklist — `- [ ]` To do, `- [x]` Done, `- [~]`/`- [-]`/`- [/]` In
progress — where the first token is a stable id (`W-14`) and `(~2d)` /
`(blocked by: W-12, W-13)` annotations give effort and dependencies.

The **Work Link** is made visible: an item shows the Agent Session working it as an
inset chip, inferred by matching the item's id against each same-project session's
git branch (`W-12` ↔ `fix/W-12-…`, `#42` ↔ `issue-42`). The chip cross-links both
ways with the session's card on the Board — a session's `plan ↗` jumps to its item,
and an item's chip jumps back to the live card.
