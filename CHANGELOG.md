# Changelog

Release notes for Riku. The section for a tagged version is what ships as that
release's notes; `Unreleased` collects what has landed since.

## Unreleased

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
