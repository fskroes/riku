# Yaplog: publishing one agent session as a readable artifact

Research date: 2026-07-28. Sources are Yaplog's own pages ([landing](https://yaplog.dev/),
[import](https://yaplog.dev/import), [install script](https://yaplog.dev/install.sh),
[privacy](https://yaplog.dev/privacy), [terms](https://yaplog.dev/terms)), the CLI
binary the site itself ships (`yaplog` v0.2.0, downloaded from
[`/download/latest?os=darwin&arch=arm64`](https://yaplog.dev/download/latest?os=darwin&arch=arm64)
and run locally for its `--help` output), the site's own first-party Stimulus source
([`transcript_pager_controller.js`](https://yaplog.dev/assets/controllers/transcript_pager_controller-b5f50346.js)),
and the seven public transcripts published by its author, listed on
[`/users/mikker`](https://yaplog.dev/users/mikker) — one of which is
[the session in which Yaplog itself was designed](https://yaplog.dev/transcripts/tr_itG3svUq),
so several product decisions below are quoted from the author's own prompts. There is
**no public repository** (a GitHub search returns only unrelated `yaplog` repos; the
binary's module path is the local `yaplog/cli`, and it self-distributes from
`yaplog.dev`, not from GitHub releases, npm, crates.io, or Homebrew), and **no
changelog, blog, or docs site** (`/changelog`, `/blog`, `/docs`, `/sitemap.xml` all
404). Yaplog is real, shipping, and closed-source.

## What Yaplog does, next to what Riku does

| Dimension | Yaplog | Riku today |
| --- | --- | --- |
| Unit of work | One **transcript** = one imported local session file, given an author-written title and description ([`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy)). No project, no day, no cross-session thread. | One **Agent Session** card, banded by Attention (`web/src/Board.tsx`); the Recap threads *across* sessions per project (`crates/board/src/recap.rs`). |
| First load, signed out | A marketing landing page. The transcript index [redirects to `/login`](https://yaplog.dev/transcripts); there is deliberately no public directory — the author's answer during design was "no directory at first but potentially yes" ([`tr_itG3svUq`, prompt at `#message-80`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)). | Local board at `localhost`, no auth, everything the machine can see. |
| Ranking | Recency only. A user page lists cards newest-first with title, description, source badge and relative age — no score, no lanes, no pagination controls ([`/users/mikker`](https://yaplog.dev/users/mikker)). | Attention-first: oldest unmet need on top (`byOldestWaiting`, `web/src/Board.tsx`), then Active, then Finished. |
| Capping | Caps *inside* the artifact, not the list: the first page renders ~40 messages, the rest arrive as lazy `messages_tail_*` turbo-frames; tool-output blocks are height-capped (`max-h-32` / `max-h-64`) with internal scroll ([`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy)). | `recap::OLDER_LIMIT = 5` with a disclosed "5 of 12" count; the same shape for the Finished band is in progress in the working tree as `FINISHED_LIMIT` (`web/src/bands.ts`, #64). |
| Navigation | The **prompt** is the index: `<nav aria-label="Prompts">`, a table of contents of every user turn, a `N/M` prompt pager with `j`/`k` keys, and a permalink per message that deep-links even into collapsed and not-yet-loaded pages ([pager source](https://yaplog.dev/assets/controllers/transcript_pager_controller-b5f50346.js), [ToC frame](https://yaplog.dev/transcripts/tr_u8pZjnFy/toc)). | The Recap's "Where I am" is a derived timeline of attention events (`web/src/Recap.tsx`); there is no prompt index. |
| Noise control | Everything between two prompts folds into one row summarised as "Thought 3 times. Used 3 tool calls. +0 -0 [Expand]"; system prompts collapsed by default, at the author's explicit request ("Make system prompts hidden by default", [`tr_itG3svUq`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)). | `crates/sessions/src/fold.rs` folds a session into one card; the Recap shows prose plus a derived timeline. |
| Human prose | Author-written title, a description on the artifact, and **notes anchored to a specific message** — a handwritten-style sticky note above the message it comments on ([`tr_u8pZjnFy`, note on `message-3`](https://yaplog.dev/transcripts/tr_u8pZjnFy)). | The journal: agent-written `done`/`next`, plus a user correction entry, append-only, latest-wins (`docs/adr/0013-agent-written-project-journal.md`). |
| Metrics | Prompts / Tool calls / Messages / Assets, plus Author, Created At, Source. **No** tokens, cost, model, duration, or per-message timestamps anywhere. | Tokens, cost (including each Sub-agent's own, priced at its own model), diff stat, branch, model, machine (`crates/sessions/src/fold.rs`, `crates/relay/src/wire.rs`). |
| Hierarchy | None. No sub-agent, sidechain, or parent notion in the UI or in the CLI binary's symbol table. | A roster on the parent: one row per Sub-agent with its Errand and its own spend, folded into the parent's totals. Codex `thread_source == "subagent"` rollouts still yield no roster row pending #76 (`crates/sessions/src/codex.rs`). |
| Where data lives | Uploaded. GitHub OAuth, Postgres, S3 for assets, originals kept ([design answers in `tr_itG3svUq`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)); "we keep your data until you delete it" ([privacy](https://yaplog.dev/privacy)). | Local-first; the journal "never leaves the machine" (ADR 0013), the Relay carries a one-way stream (ADR 0002). |

## 1. What it is for, and who it is for

Yaplog is a **publishing service for coding-agent transcripts**. Its landing page
states the motivation as a social one: "That feeling: the agent solves your problem in
30 seconds. You think, *wow, I wish everyone could see this.* Now they can"
([yaplog.dev](https://yaplog.dev/)). The four capabilities it advertises are CLI-first
imports, sharing controls (private / unlisted / public), redaction controls, and a
"smart, readable interface" that collapses noisy blocks and highlights tools
([yaplog.dev](https://yaplog.dev/)). The author's own framing in the design session is
the same, in one line: "I want to create a service that can share transcripts of chats
with you, codex" ([`tr_itG3svUq`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)).

The audience is the individual developer who wants a link to hand to someone else. It
is not a monitoring or triage tool: there is no board, no live status, no notion of a
session being in progress, no notification, and nothing that watches. Import is a
deliberate, after-the-fact act — `yaplog login`, then `yaplog import --source codex |
claude | pi` ([import](https://yaplog.dev/import)) — reading `~/.claude/projects`,
`~/.codex/sessions`, and `~/.pi/agent/sessions` (strings in the shipped binary). Note
that Yaplog supports a third agent Riku does not watch: **Pi**
([`tr_iETBAQPD`](https://yaplog.dev/transcripts/tr_iETBAQPD) is the session that added
it).

There is no pricing anywhere on the site, no team or organisation feature, and no
documented retention period beyond "until you delete it or your account"
([privacy](https://yaplog.dev/privacy)). The terms disclaim responsibility for a breach
of what you upload ([terms](https://yaplog.dev/terms)).

## 2. How it models a session, a run, and a day

A **transcript** is one imported session file, and that is the entire model. The header
of a transcript page carries exactly two definition lists — Stats (Prompts, Tool calls,
Messages, Assets) and Meta (Author, Created At, Source, Share) — plus a visibility badge
and a copy-the-link field ([`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy)).

There is **no notion of a day**, no project, and no thread spanning sessions. The CLI
does send a `cwd` field with the upload (`json:"cwd"` in the binary) and Codex's own
`~/.codex/sessions/index/by-dir` index is read, so the working directory is known — but
it is never surfaced as a grouping. The only place a directory appears on screen is
inside the original Codex system prompt, as transcript content rather than Yaplog
chrome. Nothing on the site groups seven transcripts by the four projects they came
from; the user page is a flat, newest-first list.

The first-class notion Riku lacks is **the prompt as the unit of structure**. Yaplog's
sidebar is literally `<nav aria-label="Prompts">`, its pager counts `N/M` prompts (44
prompts across 723 messages in `tr_u8pZjnFy`), and everything the agent did between two
prompts collapses into a single row. Riku's fold is per *session*; Yaplog's is per
*turn*, which yields the honest summary line "Thought 3 times. Used 3 tool calls."
Riku has no per-turn shape at all.

Two smaller first-class notions: **assets** (pasted images, counted in Stats and
extractable — "extract as screenshots are rather big these days",
[`tr_itG3svUq`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)), and **visibility**
as a property of the artifact, defaulting to private (`--visibility="private"`, values
`private, unlisted, or public`, per the CLI's own `--help`), where unlisted means an
unguessable id — "link (id should be uuid type long unguessable string)"
([`tr_itG3svUq`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)), visible in the real
ids: `tr_u8pZjnFy`.

## 3. What it shows on first load, and how it decides

Signed out, first load is the landing page. The transcript index requires login, and
**there is no ranked feed to build** — the author explicitly deferred a directory
during design ([`tr_itG3svUq`](https://yaplog.dev/transcripts/tr_itG3svUq/toc)). So
Yaplog has no answer to "what is worth showing" at the collection level: the only
ordering primitive in evidence is recency on a user page, with no cap, no score, and no
lane. Riku's Board is doing something Yaplog never attempts.

Where Yaplog *does* decide is **inside one artifact**, and that is the part worth
reading closely:

- **Progressive disclosure by default.** System prompts render collapsed; each
  assistant turn renders collapsed behind its "Thought N times. Used N tool calls.
  +A -B [Expand]" summary; each tool output is a `<details>` inside that.
- **Lazy tail.** Only the first ~40 messages are in the initial HTML; the rest load as
  `messages_tail_<seq>` turbo-frames
  ([`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy)).
- **Height caps with internal scroll** on tool output (`max-h-32`, `max-h-64`), so one
  10,000-line `ls` cannot own the page.
- **Collapse never breaks a link.** Every message folded inside a turn still emits a
  hidden anchor span (`<span id="message-5" class="hidden">`), and the pager will walk
  forward through unloaded tail frames — up to 50 — to find a hash target before giving
  up ([pager source](https://yaplog.dev/assets/controllers/transcript_pager_controller-b5f50346.js)).

The CLI's own selection step is the one place Yaplog ranks local sessions, and it is
**not documented**. The binary contains a bubbletea/fzf picker with a preview pane, a
`--match` substring filter, and an `--all` flag whose only documentation is "scan all
logs" — implying the default is narrower. The binary references a working-directory
index and a `workingDirForLog` helper, which suggests the default is scoped to the
current directory, but neither the site nor `--help` says so and I could not confirm it
by running the picker. Treat that as unknown.

## 4. The read-later experience, and where it beats the Recap

Yaplog's read-later surface is a single long page, and it beats Riku's Recap on four
specific things:

1. **A prompt-level table of contents.** Every user turn is an entry, labelled with the
   prompt's own text, with an active-section indicator driven by an IntersectionObserver
   ([ToC frame](https://yaplog.dev/transcripts/tr_u8pZjnFy/toc), [pager
   source](https://yaplog.dev/assets/controllers/transcript_pager_controller-b5f50346.js)).
   Riku's Recap card lists `done` lines grouped by day and a derived timeline; neither
   is addressable, and neither says "you asked 44 things".
2. **Prose anchored to a point, not to the card.** The note on `message-3` of
   [`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy) is a handwritten-style
   marginal note attached above one specific message: "Claude (the web version) wrote
   this entire description. Here's the original chat", with a link out. Riku's
   correction box (#59, landed in `bf6d3ff`) appends a `who:"user"` entry that answers
   the *whole card*; it cannot say "this step, here, is where it went wrong".
3. **A per-turn effort summary.** "Thought 3 times. Used 3 tool calls." is a cheap,
   legible unit of "how hard was this bit", derived from the transcript and needing no
   agent cooperation. Riku's equivalent numbers exist only per session.
4. **Deep-linkable permalinks that survive folding and pagination**, as above.

Where the Recap is already ahead, and should stay ahead:

- **Threading across sessions.** ADR 0013's thread-first recap groups an effort that
  spans several sessions per project; Yaplog has exactly one session per artifact and no
  concept above it.
- **A next step and a resume command.** Yaplog has no "what next" at all. `CardResume`
  / `OlderResume` (`crates/board/src/recap.rs`) have no counterpart.
- **Prose checked against ground truth.** Riku deliberately keeps the derived timeline
  beside the agent's prose so a wrong summary is catchable (ADR 0013,
  `web/src/Recap.tsx`). Yaplog's title, description, and notes are unchecked author
  claims; the artifact contains the transcript, but nothing juxtaposes them.
- **Disclosed caps.** Riku says "5 of 12" (`web/src/bands.ts`, `recap::OLDER_LIMIT`).
  Yaplog's per-turn fold hides an unstated number of messages behind "[Expand]" — the
  count of *thoughts* and *tool calls* is shown, which is a partial disclosure, but the
  lazy tail has no "40 of 723" anywhere.

One cautionary finding, relevant to ADR 0013's "prose is inherently more sensitive"
argument. Yaplog's redaction is **manual and post-hoc**: per-block "Redact output",
"Unredact output", and "Redact image" PATCH controls that replace the block with a
dotted placeholder. There is no evidence of automatic secret scanning at import, and the
landing page's promise to "hide secrets, paths, emails, and other sensitive bits"
([yaplog.dev](https://yaplog.dev/)) describes this manual control. The consequence is
visible in the author's own public transcript: the collapsed system prompt of
[`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy) publishes his full skills
inventory, dotfile paths, home-directory project path, and the Codex environment block
(`Approval policy: never`, `Sandbox mode: danger-full-access`). Collapsed is not
redacted.

## 5. Hierarchy: parent agent to sub-agent

**It does not represent hierarchy.** Stated plainly, because it bears on #67:

- The shipped CLI binary contains no `sidechain`, `subagent`, `sub-agent`, `parentUuid`,
  or equivalent string, in either its data-model or its Claude-specific code paths. It
  reads `~/.claude/projects` and uploads the session file; nothing suggests it separates
  a sidechain from the main conversation.
- None of the seven public transcripts renders any parent/child relationship. The only
  nesting the viewer has is the turn fold: an indented, left-bordered block of thoughts
  and tool calls under one collapsed summary.
- The closest thing to a plan or structure is Codex's `update_plan` tool, which renders
  as a tool call with its raw JSON payload
  ([`tr_u8pZjnFy`](https://yaplog.dev/transcripts/tr_u8pZjnFy)) — a plan shown as
  output, not as structure.

I could not test the one interesting case: a Claude transcript containing `Task`
sub-agent sidechains. The only public Claude-sourced transcript
([`tr_5nux18hL`](https://yaplog.dev/transcripts/tr_5nux18hL)) has 2 prompts and 0 tool
calls. So "Yaplog flattens Claude sidechains into the linear message stream" is a
reasonable expectation from how it uploads, but it is *not* verified — the site
documents nothing about sub-agents either way.

## Verdicts

**Adopt — the prompt as an index unit, in the Recap.** Riku's `web/src/Recap.tsx`
timeline is a list of derived attention events; add a prompt spine (count and
addressable rows) so "Where I am" can be read as "these are the things you asked".
Requires nothing from the journal and no agent cooperation — it is derivable in
`crates/sessions`, like the timeline already is. Surface: **Recap**. No issue yet.

**Adopt — the per-turn effort summary, "Thought N times, used N tool calls".** Cheapest
real win here. Riku already computes richer numbers per session in
`crates/sessions/src/fold.rs`, including a real `DiffStat`; per-turn would make the
Recap's timeline legible without prose. Note that Yaplog's own `+A -B` diff read `+0
-0` in *every* turn of *all seven* public transcripts, including sessions that clearly
wrote thousands of lines — the affordance is there and unpopulated, so Riku would ship
the better version of it. Surface: **Recap**.

**Adopt — anchored user prose.** Extend the correction box (#59, `bf6d3ff`) so a
`who:"user"` entry can name the timeline row it answers, the way a Yaplog note attaches
above one message. Keep append-only and latest-wins exactly as ADR 0013 specifies; this
is an anchor field, not an edit. Surface: **Recap** (journal record schema — a `v`-bump
case, which ADR 0013 already provides for).

**Adopt — collapse that does not break addressing.** Yaplog's hidden anchor spans plus
its "walk forward through unloaded frames to find the hash" pager are the right pattern
for any Riku surface that caps or folds. Directly relevant to the open question in
**#64**: Riku's `finishedBand` already handles this correctly by pushing a `focusId`
session past the cap (`web/src/bands.ts`), which is the same principle. Surface: **Board
bands**.

**Don't adopt — #64 needs nothing from Yaplog.** The disclosed cap is already being
built the right way: `FINISHED_LIMIT = 5` and `finishedLine` produce "5 of 12" in
`web/src/bands.ts` (uncommitted working-tree work at the time of writing, wired into
`web/src/Board.tsx`), mirroring `recap::OLDER_LIMIT`. Yaplog contributes no better idea
— it caps inside an artifact and discloses less than Riku does. The one transferable
detail is the *lazy tail* (load the remaining Finished sessions on demand rather than
never), and it is not worth it for a band of session rows. On #64's open questions:
keep the cap client-side where `focusId` can override it, as `bands.ts` already does,
and answer "should Attention and Active get caps too" with no — Yaplog's evidence is
that capping belongs to history, not to the live queue.

**Don't adopt — publishing, visibility tiers, upload, and author-written titles.** The
entire spine of Yaplog is "get this off the machine and give someone a link". That is
the exact thing ADR 0002 (one-way stream, no remote control) and ADR 0013 (the journal
never leaves the machine, `riku journal --purge` deletes it) rule out. A human-authored
title and description are publishing artefacts; Riku's prose comes from the agent that
did the work and is corrected by the user, which is a different and better contract for
a private board. Surface: **Recap**, and it stays local.

**Don't adopt — Yaplog's redaction model, but take the warning.** Per-block manual
redaction only makes sense once content has left the machine. The transferable finding
is the failure mode: Yaplog's own public transcript ships its author's system prompt,
paths, and sandbox posture behind a `<details>`. It is evidence for ADR 0013's stance
that journal prose is more sensitive than the ledger's ids, and for keeping the Recap
off both the Collector and the Relay. Surface: **Recap** (no change; a reason not to
change).

**Don't adopt — nothing for Work Items or the Work Link (#66).** Yaplog has no
issue, task, kanban, or status concept of any kind, and no live-session notion at all.
There is no external precedent here for whether a live Work Link should *derive* Doing
(`WorkStatus::Doing` in `crates/sessions/src/work.rs`) or sit beside the source's
status as a separate `LinkedSession` signal (`crates/board/src/http.rs`). **#66** must
be decided on Riku's own terms; this research contributes nothing to it, which is worth
recording so nobody looks again.

**Don't adopt Yaplog's model for #67 — it has none — but take its rendering pattern.**
On hierarchy Yaplog is a dead end: no parent/child anywhere, and for Codex it never
even had to decide, since it publishes whatever single rollout you point it at. Riku's
current handling (the Sub-agent roster, each row's cost folded into the parent,
Codex `thread_source == "subagent"` rollouts suppressed in
`crates/sessions/src/codex.rs`) has no counterpart to learn from. What *is* reusable is
the turn fold's shape: an indented, left-bordered, collapsed child block under its
parent, summarised by counts, with hidden anchors so a child stays permalinkable while
folded. That is a good answer to #67's "is a sub-agent ever its own card" — it can be a
foldable child block under the parent's row instead, keeping Codex's
no-card-of-its-own decision intact while making the relationship visible. Surface:
**sub-agent handling**, **#67**.

**Don't adopt now — Pi as a third source.** Yaplog reads `~/.pi/agent/sessions`
alongside Claude Code and Codex ([import](https://yaplog.dev/import)), which is a real
signal that a third local agent is worth watching eventually. No verdict beyond noting
it; Riku's source support is an ADR-level decision, not a research finding.

## What primary sources could not answer

- **The CLI picker's default scan scope.** `--all` is documented only as "scan all
  logs"; the default is undocumented on the site and in `--help`.
- **How Claude `Task` sidechains render.** No public Claude transcript exercises them.
- **Whether anything ranks or caps a signed-in user's own transcript index.**
  `/transcripts` requires login and was not accessed.
- **Pricing, retention duration, team features.** Not documented anywhere on the site.
