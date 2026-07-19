# Agent Board

A local mission-control board for AI coding agent sessions. It watches your local
Claude Code **and** Codex CLI transcripts and renders every session live on an
attention-first board (issues #2, #5).

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
