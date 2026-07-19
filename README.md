# Agent Board

A local mission-control board for AI coding agent sessions. This is the **C1
walking skeleton** (issue #2): it watches your local Claude Code transcripts and
renders every session live on an attention-first board.

```
Cargo.toml            workspace
crates/collector/     lib — the Claude Code Session Source: discovery,
                      transcript tailing, the Session model, status heuristic
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
(default `~/.claude/projects`), `--web-dist <dir>` (default `web/dist`).

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

Each Claude Code transcript (`~/.claude/projects/<project>/<uuid>.jsonl`) is one
**Agent Session**. The collector scans transcripts touched in the last 24h, tails
each incrementally (byte-offset per file, re-parsing on truncation), and folds
`user` / `assistant` entries into a `Session` — skipping sidechain (subagent)
traffic and tolerating unknown fields / schema drift. Status is mtime-based for
C1: an unanswered `tool_use` in the newest entry is **Attention**, a fresh file is
**Active**, and a quiet one (≥15m) is **Finished**.

The board serves a full snapshot at `GET /api/sessions` and streams full-Session
updates over SSE at `GET /api/events` (`session` / `removed` events; the client
upserts by `id` and re-syncs the snapshot on reconnect). See
`docs/adr/0005-*` for the UI decision and `CONTEXT.md` for the domain language.
