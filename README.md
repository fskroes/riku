# Riku — Agent Board

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/fskroes/riku?sort=semver)](https://github.com/fskroes/riku/releases)
[![Release build](https://github.com/fskroes/riku/actions/workflows/release.yml/badge.svg)](https://github.com/fskroes/riku/actions/workflows/release.yml)
[![Source-install smoke test](https://github.com/fskroes/riku/actions/workflows/homebrew-smoke.yml/badge.svg)](https://github.com/fskroes/riku/actions/workflows/homebrew-smoke.yml)

Riku is a local, read-only mission-control board for Claude Code and Codex CLI
sessions. It puts the sessions that need your attention first, so you can see what
to respond to instead of searching through terminal windows.

![The board: the one session waiting on you on top, then Running in the background, Up next, and Finished](docs/images/board.png)

## At a glance

- **Attention first.** Sessions needing a human appear before active and finished
  work; staleness stays a card hint rather than becoming an alert.
- **Local and read-only.** Riku reads local transcripts. A session can be resumed
  in a local Terminal window, but Riku never sends commands to an agent remotely.
- **Context with the work.** Cards show the model, branch, tokens, live git diff,
  and an optional cost estimate. The Work Items view connects a live session to the
  work it is carrying out.
- **The day, not just the moment.** With the opt-in project journal, the agent
  writes what it finished and what is next before its session ends, and the Recap
  view reads that back as threads of effort you can answer in your own words.

## Install on macOS (Apple Silicon)

```sh
brew tap fskroes/riku
brew install riku
riku
```

Then open <http://localhost:4242>. Riku scans the default Claude Code and Codex CLI
session locations, so a solo board needs no configuration. You should see your
recent sessions arranged with Attention first.

Apple-Silicon binaries are published through Homebrew. On an Intel Mac, use the
[source build](#build-from-source) below.

## Common setups

### Solo board

Run `riku` and leave it running while you work. To use non-default transcript
locations, pass `--root <dir>` for Claude Code or `--codex-root <dir>` for Codex
CLI. `riku --help` lists the remaining board options.

### Team or multi-machine board

For sessions on other machines, run one **Relay** behind a TLS-terminating reverse
proxy, a **Collector** on each machine, and point the Board at the Relay. The Relay
only carries session state; it never carries agent-control commands.

```sh
# On the Relay host, behind a TLS proxy that forwards to 127.0.0.1:4343.
riku relay --addr 127.0.0.1:4343 --token "$RELAY_TOKEN"

# On each machine whose sessions you want to collect.
riku collect --relay https://relay.example.com --token "$RELAY_TOKEN"

# On the machine running the Board.
riku --relay https://relay.example.com --token "$RELAY_TOKEN"
```

Remote Relay URLs must use `https://`; plain HTTP is accepted only for a loopback
host such as `http://localhost:4343`. For the proxy configuration, certificate
requirements, and token handling, see [Deploying a multi-machine Relay](docs/relay-deployment.md).

You can save Relay and path settings in `~/.config/riku/config.toml`:

```sh
riku config set relay.url https://relay.example.com
riku config set relay.token "$RELAY_TOKEN"
```

Explicit flags take precedence over `RELAY_URL`, `RELAY_TOKEN`, `RIKU_ROOT`, and
`RIKU_CODEX_ROOT`, which in turn take precedence over the config file.

## Recap and the project journal

The **Recap** view answers "what did I do today, and what is the next best step?".
It reads an append-only **project journal** that the agent writes — one entry per
session, as the last thing it does before stopping — so "what is done" and "what is
next" come from the party that actually did the work rather than from a guess.

![Recap threads: Done so far by day, Where I am, To go further, and a resume command](docs/images/recap.png)

The journal is off until you turn it on, and Riku neither reads nor writes it
before then:

```sh
riku config set journal.enabled true
```

Entries are written by the agent, not by Riku, so they need a stop hook. A Claude
Code `Stop` hook ships in [`hooks/claude-code/`](hooks/claude-code/) — its README
covers the install and the one permission rule it needs. An installed hook keeps
writing whether or not Riku is reading, so uninstall it if you want nothing on
disk. A project whose agent is not wired simply has no prose, and its card falls
back to the timeline Riku derives from transcripts.

Each card ends in a copy-paste command to resume that thread in a clean session.
When the agent's account is wrong, answer it — from the card, or from the CLI:

```sh
riku journal note . "temps.py is NOT done — I also need Kelvin"
riku journal note . "that's fine, carry on" --handoff on-track
riku journal --purge                    # delete every journal file on this machine
```

Nothing is edited or deleted: your entry is appended, and the last word wins
whoever spoke it. The next session's stop hook reads your answer back, which is
what makes the journal a conversation. Entries stay on this machine — the Relay
never carries them ([ADR 0013](docs/adr/0013-agent-written-project-journal.md)).

## Work Items

The **Work Items** view shows one project's work as a kanban and dependency graph.
It uses `WORK.md` when present; otherwise it reads GitHub Issues through `gh`.

![Work Items kanban for one project: To do, In progress, and Done columns](docs/images/work-items.png)

`WORK.md` is a Markdown checklist. Use `- [ ]` for To do, `- [~]`, `- [-]`, or
`- [/]` for In progress, and `- [x]` for Done. The first token is the stable item
id; optional `(~2d)` and `(blocked by: W-12, W-13)` annotations add effort and
dependencies.

```md
- [ ] W-12 Add the Relay status pill (~1d)
- [ ] W-13 Deploy the Relay (blocked by: W-12)
```

When a same-project session is on a branch containing an item's id — for example,
`fix/W-12-relay-pill` — Riku displays the most recently active matching session as
that item's **Work Link**. This association is inferred from the current branch
name only: it is not stored or manually editable.

![The dependency graph: critical path flagged, done items green](docs/images/work-graph.png)

## Build from source

From the repository root, install the frontend dependencies and build the embedded
UI before compiling Riku:

```sh
cd web && npm ci && npm run build && cd ..
cargo run -p riku
```

The board runs at <http://localhost:4242>. `web/dist` must exist at compile time;
the build fails with this same command when it is missing.

For UI development with hot reload, run the API and Vite separately:

```sh
cargo run -p riku             # API on :4242
cd web && npm run dev         # UI on :5173, proxies /api to :4242
```

## Verify a checkout

```sh
cargo test
cd web && npm run build
```

## How it works

Each transcript becomes an **Agent Session** through one of two **Session Sources**:
Claude Code or Codex CLI. The Board watches local sessions directly; a Collector
watches a remote machine and pushes the same session state to a Relay. The board
merges those local and remote updates into one attention-first stream.

The board exposes a snapshot at `GET /api/sessions`, live updates through
Server-Sent Events at `GET /api/events`, and the journal-derived recap at
`GET /api/recap`. See [CONTEXT.md](CONTEXT.md) for the project's domain language and
[the architecture decisions](docs/adr/) for the rationale behind local-first
operation, the read-only Relay, Explainable Attention, card stats, the project
journal, and distribution.

## Contributing

Issues, PRDs, and triage labels live in [GitHub Issues](https://github.com/fskroes/riku/issues).
Before opening a pull request, read [CONTEXT.md](CONTEXT.md) for the domain language
and [AGENTS.md](AGENTS.md) for the repository conventions, then verify your checkout
with `cargo test` and `cd web && npm run build`.

## License

Riku is released under the [MIT License](LICENSE).
