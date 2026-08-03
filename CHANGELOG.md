# Changelog

Release notes for Riku. The section for a tagged version is what ships as that
release's notes; `Unreleased` collects what has landed since.

## Unreleased

### The Sub-agent roster is a fold you open, not a panel you hover

A card that fanned work out now carries a line under its own — `⑃ 3 sub-agents in
all, none running` — that opens into the roster and stays open until you close it.
The hover panel it replaces could not be reached by touch, could not be held open
while you read one Sub-agent's Errand against another's or against the parent's own
numbers, and closed itself on the smallest pointer drift. The line is a button: it
is reachable from the keyboard and says whether it is open or shut.

Every fold starts shut, so a first paint looks as it did, and nothing but you opens
or closes one — a poll, a resume, or a Sub-agent ending never moves a fold under
your eye. Which fold is open belongs to the card and is not remembered: a reload
closes them all. The counts beside **Running in the background**, **Up next** and
**Finished** are unchanged, because they count Agent Sessions, and one Agent Session
is still one row there however many Sub-agents it opens beneath it.

## 0.3.0

### A Claude Sub-agent that finished while its parent was busy now says so

A roster row could sit on Running forever, and its card would sit under **Running in
the background** long after everything on it had stopped — a session quiet for five
hours still reading as live work.

Claude Code writes a Sub-agent's completion into the transcript differently
depending on what its parent was doing at the time. If the parent was idle, the
notification arrives as a turn. If the parent was mid-turn — which is exactly what a
session that has just fanned work out is — the notification is queued instead, and
the turn is never written. Riku only read the turn, so it never heard about the
second case: **33 of 92** Sub-agent spawns on the machine this was found on, leaving
**20 of 41** fan-out cards with a row stuck Running. A completion is now read from
whichever record first states it, rather than only from the turn — and only from the
record that states it, not from the queue's later echo of the same news, which would
have re-finished a Sub-agent that had been sent back to work.

Nothing about the outcome changed: the word on the row is still the source's own
(`completed`, `failed`, `stopped`, `killed`), never guessed, and a Sub-agent that
ended without one still ends unworded.

### Codex cards cost roughly double, and that is the number becoming true

Riku now counts what a Codex CLI session's Sub-agents spend, the way it already
counted a Claude Code session's. A Codex card that fans work out will show
noticeably higher tokens and cost than it did in 0.2.0 — **roughly twice** — with
the Sub-agent roster on the card as the disclosure of whose spend it was.

Nothing got more expensive. That spend was always real; it was being attributed
to nothing, because Codex Sub-agent rollouts were dropped on the way in under a
comment asserting Codex had no Sub-agent concept. It has one, and a richer one
than Claude's. Measured across this machine's rollouts on 2026-07-30, over the 35
Codex sessions that spawned children:

| | |
| --- | --- |
| Sub-agent input tokens, as a share of their parents' own | **98%** |
| Worst single card | **3.61×**, from 8 children |
| Input tokens previously attributed to nothing | **227,473,226** |

(ADR 0014 recorded 106% a day earlier, over 75 rollouts rather than 79. It is a
live corpus, so the share moves; the worst case and the conclusion did not.)

Claude Code cards do not move: their Sub-agent spend has been counted since
0.2.0.

Alongside it, a Codex Sub-agent now appears as a row on its parent's card — with
its depth, what it spent, and `completed` when it reached its terminal event —
and never as a card of its own, because nobody can approve, answer, or resume a
Sub-agent directly, only the session that sent it (ADR 0014). Rows are
unlabelled: Codex names its Sub-agents `Dirac` and `Euclid`, which says nothing
about the work, and a blank beats a label that merely looks like content.

One fix rides along that also affects ordinary Codex cards: a rollout that forks
replays the meta of the thread it forked from into its own history, and Riku was
letting that later record rename the session. The first `session_meta` a rollout
states is now the one that identifies it — the id in the filename, in 193 of 193
observed rollouts.

## 0.2.0

Released 2026-07-28. See the [GitHub release](https://github.com/fskroes/riku/releases/tag/v0.2.0).
