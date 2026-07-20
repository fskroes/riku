# Agent Board (riku)

A local mission-control board for AI coding agent sessions. It watches your local
Claude Code **and** Codex CLI transcripts and renders every session live on an
attention-first board (issues #2, #5), with a per-project **Work Items** view that
links each session to the plan it is carrying out (issue #1, C4).

![The board: Needs You on top, Running below, Finished last](docs/images/board.png)

## Why this exists

Once you run more than two coding agents at a time, you acquire a problem that has
nothing to do with software and everything to do with psychology. Ten agents are
ten brilliant interns working in ten separate rooms with the doors closed. Nine of
them are fine. One of them has been standing politely at the door for forty
minutes, holding a question, and you are the bottleneck without knowing it.

The instinctive fix is a dashboard. But most dashboards fail for a wonderfully
human reason: they are designed to show you *everything*, and a screen that shows
everything tells you nothing. Nobody ever missed a flight because the departures
board was too small; they missed it because their flight looked exactly like all
the others.

So the board makes precisely one editorial decision, and makes it ruthlessly:
**what needs you goes on top.** An agent waiting for approval or dead on an error
is the headline. Agents happily burning tokens are the middle of the paper.
Finished agents are the archive. You don't scan the board — the board scans you.

Three small perceptual tricks do most of the work:

- **Attention is a status, not a colour.** A card only moves to *Needs You* for a
  hard signal — an unanswered question, an error exit. Staleness is a hint on the
  card, never a promotion. Cry wolf once and every alert is wallpaper.
- **One action, and it's the right one.** `Review →` doesn't show you a log — it
  opens a terminal *inside the session*, resumed, in the right directory. The cost
  of responding drops to one click, so you actually respond.
- **Prices change behaviour.** Every card can show a `$ est.` figure. Not because
  the number is precise (it says "est." for a reason) but because a visible cost is
  the difference between "hm, that agent has been thinking for two hours" and doing
  something about it. Subscription users can switch it off — a price you don't pay
  is just noise.

It's local-first, read-only by design, and never controls an agent remotely
(ADR 0002). It is a mirror, not a remote control.

```
Cargo.toml            workspace
crates/sessions/      lib — the Session Sources (Claude Code + Codex CLI) behind a
                      shared trait: discovery, transcript tailing, the Session
                      model, status heuristic, live git diff enrichment
crates/session-engine/ lib — shared Tokio runtime: discovery, watch, refresh,
                      enrich + stamp, local snapshots and events
crates/board/         lib — axum server: embedded UI + /api/sessions + SSE
crates/relay/         lib — Relay + Collector runtime and shared wire codec
                      the board subscribes to a Relay
crates/riku/          bin — `riku` umbrella CLI (Board, Collector, Relay, Config)
web/                  React 18 + Vite 5 + TypeScript board (attention stream)
```

## Run it

```sh
# 1. Build the UI (once, and after web/ changes). Cargo embeds this output.
cd web && npm install && npm run build && cd ..

# 2. Run the board
cargo run -p riku             # then open http://localhost:4242
```

Flags: `--port <n>` (default 4242, binds `127.0.0.1` only), `--root <dir>`
(Claude Code projects, default `~/.claude/projects`), `--codex-root <dir>` (Codex
CLI sessions, default `$CODEX_HOME/sessions` or `~/.codex/sessions`), and
`--web-dist <dir>` (a development-only disk override). Without `--web-dist`, riku
serves the UI compiled into its binary from any current directory. A missing Codex
root degrades gracefully — Claude sessions still show.

`web/dist` must exist before Cargo builds so it can be embedded; the build fails with
a direct `npm ci && npm run build` instruction if it is absent. The old 503 page is
retained only when an explicit `--web-dist` path does not exist.

## Install on macOS

The release configuration is ready to publish a source-build formula followed by
tagged bottles to `fskroes/homebrew-riku`. The source repository must be public (or
the release artifacts hosted publicly) before Homebrew can fetch it; until then,
build locally with the commands above. Once published, installation is:

```sh
brew tap fskroes/riku
brew install riku
riku
```

Use `riku --help` to discover the umbrella commands. `riku collect` is the
per-Mac Collector; `riku relay` remains available for local development but is not
installed as a Homebrew service. Configure an unattended Collector once, then use
Homebrew to manage it:

```sh
riku config set relay.url https://relay.example.com
riku config set relay.token "$RELAY_TOKEN"
brew services start riku
```

The Relay URL must be `https://` for a remote hub; riku refuses to persist or connect
to a remote `http://` URL so the shared token and Session stream are never sent in
cleartext. Plain `http://localhost` is accepted only for a single-machine loopback
setup. See [Team / multi-machine](#team--multi-machine-c7).

Config is stored at `~/.config/riku/config.toml` with `0600` permissions. Values
resolve in this order: explicit flags, `RELAY_URL` / `RELAY_TOKEN` / `RIKU_ROOT` /
`RIKU_CODEX_ROOT`, then the Config file. `brew services stop riku` stops the
Collector; its logs use Homebrew's standard `~/Library/Logs` location.

## Team / multi-machine (C7)

Solo use needs nothing above. To see sessions from other machines — a second laptop,
a teammate's box — run a **Relay** once (anywhere reachable) and a **Collector** on
each machine, then point the board at the Relay. A single shared token gates
everyone; the Relay holds only live in-memory state (nothing to back up) and is
strictly one-way read-only — it transports session state, never commands (ADR 0002).

Remote transport is encrypted, and that is enforced, not advised: the Collector and
board require an `https://` Relay URL. `riku relay` is a **loopback-only development
server** — a real multi-machine Relay is a loopback riku process behind a
TLS-terminating reverse proxy that presents the certificate. See
**[docs/relay-deployment.md](docs/relay-deployment.md)** for one supported shape and
its certificate, forwarding, and token requirements.

```sh
# A remote Relay sits behind a TLS proxy (nginx/Caddy) → 127.0.0.1:4343.
# Run the Relay bound to loopback; the proxy terminates TLS:
cargo run -p riku -- relay --addr 127.0.0.1:4343 --token "$RELAY_TOKEN"

# On each machine whose agents you want to see (headless, no UI):
cargo run -p riku -- collect --relay https://relay.example.com --token "$RELAY_TOKEN"

# Point your board at the Relay (still binds localhost; local sessions keep working):
cargo run -p riku -- --relay https://relay.example.com --token "$RELAY_TOKEN"
```

For a single-machine setup where Board, Collector, and Relay all run on the same host,
plain `http://localhost:4343` is accepted — the one loopback exception to the HTTPS
rule.

The token may also come from the `RELAY_TOKEN` environment variable for all three.
The Collector reuses the same `--root`/`--codex-root` flags as the board and
`--machine <name>` to override the host label. Remote sessions flow into the same
Active / Attention / Finished columns, each card labelled with its machine; the
topbar pill shows `relay ✓ · N machines`. If a Collector goes offline its cards
disappear; on reconnect it re-pushes its state.

## Develop the UI with hot reload

```sh
cargo run -p riku             # API on :4242
cd web && npm run dev         # UI on :5173, proxies /api to :4242
```

## Test

```sh
cargo test                    # sessions unit tests + board integration tests
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

## Card stats (C5)

Every card carries two more stats beyond model / branch / tokens / age:

- **diff `+/-`** — live git lines changed for the session's repo: the branch's work
  since it left the repo's default branch (`origin/HEAD`, else `main`/`master`),
  **plus** uncommitted working-tree edits. It is live repo state, not transcript
  data, so the collector leaves it empty and the **board** fills it (the same seam
  as Work Links), shelling out to `git` with a short per-directory TTL cache. A cwd
  that is not a git checkout simply shows no diff.
- **cost `est.`** — an estimate of `tokens × the model's public list price`,
  computed source-agnostically in the collector and always labelled "est.". A
  header toggle (`$ est. on/off`, remembered in `localStorage`) hides it for
  **subscription** users, who pay no marginal per-token cost; an unpriced model
  shows no cost. See `docs/adr/0006-*`.

## Work Items (C4)

The **Work Items** tab shows one project's plan at a time, rendered two ways over
the same item set — a **To do / In progress / Done** kanban and a **dependency
graph** laid out by blocked-by depth. A project selector switches projects (drawn
from the live sessions), and a source badge shows whether the items came from
`WORK.md` or GitHub Issues.

![Work Items kanban for one project, sourced from GitHub Issues](docs/images/work-items.png)

The graph view (PR #24) is where dependencies earn their keep: the **critical
path** — the longest chain of unfinished, blocking items — is highlighted, so the
question "what should an agent pick up next?" answers itself. Hovering an item
lights up its full **lineage** (everything it blocks and everything blocking it),
items with a live Agent Session get a pulsing **agent ring**, an item that needs
you gets an attention ring, and the canvas **pans** for plans bigger than a
screen.

![The dependency graph: critical path flagged, done items green](docs/images/work-graph.png)

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
