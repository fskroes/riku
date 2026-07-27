# 0013 — Agent-written project journal

## Status

Proposed (2026-07-27).

## Context

Riku answers "what needs my attention *now*" but not "what did I do this morning,
what did I do yesterday, and what is the next best step per project?" The board is
a snapshot; the day is not recorded. A UI prototype
(`web/prototype/activity-recap.html`) explored four surfaces and settled on a
**day board** (variant D): a day sidebar on the left, and on the right one card
per project with three columns — **Done**, **Where I am** (a small timeline), and
**To go further** — plus a copy-paste command to resume the work in a clean
session.

Two of those three columns cannot be derived from transcripts. Riku already parses
tool calls into attention events (`crates/sessions/src/attention.rs`), so a raw
timeline is free. But "what is *done*, in a sentence" and "the *next best step*"
are interpretation, and the only party that reliably holds that interpretation is
the agent that just did the work. Riku inferring a next step from the last blocking
tool call would be guessing, and a wrong guess in the resume command is worse than
no command.

This crosses two lines Riku has been careful about:

- **A second durable writer.** ADR 0012 introduced Riku's first durable state
  beyond `config.toml` (the append-only attention ledger) and flagged that as the
  line worth an ADR. The journal is a second durable file — and unlike the ledger,
  **Riku does not write it**. Something outside Riku writes into Riku's data
  directory, which is a further step and deserves its own record.
- **Prose, not ids.** The ledger is deliberately ids-and-enums-only — `cat`-able
  in front of a security reviewer, nothing to leak (ADR 0012). The journal carries
  free text: what was done, what is next, and a resume command. It is inherently
  more sensitive, so the privacy posture has to be stated, not inherited.

Riku's read-only, local-first stance (ADR 0002: the Relay carries a one-way stream
and never transports commands) constrains the shape of any answer here.

## Decision

Add an **agent-written project journal**: an append-only, opt-in log that the
**agent writes on stop**, the **human can answer**, and Riku never writes on its
own behalf. The journal is a conversation between the user and the agent, not an
authoritative agent narrative.

**Who writes, and when.** The agent appends **one entry as the last thing it does
before a session ends** — driven by a stop hook (Claude Code `Stop`, the Codex
equivalent), not by Riku. On-stop is chosen over a separate "scribe" agent that
re-reads transcripts: the doer knows precisely what it did and what remains, so its
entry is a report, not a reconstruction. The cost — every cooperating agent must be
wired with the hook — is accepted, and a session whose agent is not wired simply
has no journal entry (the card falls back to the derived timeline).

**Storage.** One newline-delimited JSON file per project at
`$XDG_DATA_HOME/riku/journal/<project>.jsonl` (else
`~/.local/share/riku/journal/<project>.jsonl`), created mode `0600` like
`config.toml` and the ledger. `<project>` is a stable slug of the workspace path.
Append-only; old entries *are* the "what did I do yesterday" history. Size-capped
and rotated, with a version tag per record for future format changes.

**Records.** One entry kind:

```
{v, project, session, at, who:"agent"|"user", status:"track"|"review"|"blocked",
 done:[string], next:string, resume:{sid, instruction}}
```

`done` and `next` are the agent's own prose. `resume.sid` is the session id Riku
already knows from transcripts; the card renders `claude --resume <sid>
"<instruction>"` for the human to copy. `status` maps to the card's pill.

**Read and render.** Riku reads the journal and renders the day board. The card's
Done / next / resume command come from the **latest entry not newer than the
selected day**; the derived transcript timeline stays visible beside the prose, so
a wrong agent summary is caught against the real events — the journal never
replaces ground truth, it annotates it.

**Disagreement.** The agent's entry is an interpretation, and it will sometimes
be wrong. The user does not edit or delete it — append-only holds for both
voices. Instead the user **appends a correction entry** (`who:"user"`), either
from the card itself or via `riku journal note <project> "<text>"`. When Riku
appends that entry it is acting as the user's pen — an explicit user action, not
Riku writing state on its own, so the read-only posture survives. Resolution is
simple recency: **the latest entry wins** for status and next step regardless of
author. Closing the loop is the agent's job: the stop hook hands the agent the
journal tail, so a correction is read before the next entry is written — the
agent answers the user's answer, which is what makes it a conversation rather
than two monologues.

**Privacy and control.**

- **Opt-in, off by default** (`riku config set journal.enabled true`), separate
  from the ledger toggle.
- **Never leaves the machine** — not carried by the Collector or Relay, ever
  (ADR 0002 / 0012).
- **`riku journal --purge`** deletes the files in one command.

## Trade-offs

- **Riku renders text it did not produce.** The journal is untrusted input to the
  board: escape it on display, never execute it. The resume command is shown for
  the human to copy — Riku never runs it, consistent with ADR 0002's no-remote,
  and no-auto-run-locally either.
- **The trust boundary is the local account.** Any local process can append to a
  `0600` file the user owns — the same boundary as `config.toml`. That is
  acceptable for a local-first tool; a shared secret like the Relay's is
  deliberately *not* added, because the journal never crosses a machine.
- **Stale next steps.** An agent that dies uncleanly (crash, `kill -9`) writes no
  stop entry, so the card shows the previous "next" until the session runs again.
  The card labels entry age (`latest 2h ago`) so staleness is visible, not hidden.
- **Interpretation can be wrong.** Unlike the ledger, which measures the reducer,
  the journal reports an agent's judgment and can misstate what happened. Keeping
  the derived timeline alongside is the mitigation; the journal is a convenience
  layer over ground truth, not a replacement for it.
- **Second durable surface.** Adds a data directory of prose files with rotation
  and corruption tolerance. Kept boring — flat JSONL, one file per project,
  `cat`-able — to stay faithful to the read-only, local-first posture.
