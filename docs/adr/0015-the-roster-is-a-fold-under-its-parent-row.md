# 0015 — The Sub-agent roster is a fold under its parent's row, not a hover panel

## Status

Accepted (2026-08-03).

Supersedes exactly one clause of
[ADR 0014](0014-sub-agents-fold-as-their-own-projection.md): the Considered
option *"Rendering a foldable child block under the parent's row (the Yaplog
pattern from the #65 research)"*, which 0014 rejected. Everything else 0014
decided stands unchanged — a Sub-agent is still never a card, still its own
projection type, still attached to the root Agent Session, still incapable of
moving its parent to Attention, and its spend still folds into the parent's
totals. This ADR changes where the roster is drawn and nothing about what it is.

## Context

0014 rejected the fold on one ground, quoted in full:

> `CONTEXT.md`'s **Band** is *"a labelled, counted run of rows… the count says
> how much is there"*. Expanding seven child rows inside a Band makes its count
> stop describing what is on screen — the exact failure #64/#65 established the
> `5 of 12` discipline to prevent.

That objection reads the Band's count as counting *rows on screen*. It counts
**Agent Sessions**, and the `5 of 12` discipline exists because a capped list of
Agent Sessions can read as the whole set of Agent Sessions. A Sub-agent is not an
Agent Session — 0014 is the ADR that made that distinction load-bearing at the
compile boundary. A nested list of a different kind of thing, drawn inside one
row and visibly subordinate to it, does not make the Band's count wrong: the
Band's run is still one row per Agent Session, and its count still says how many
Agent Sessions are there.

What 0014 chose instead has not held up. Its own **Deliberately out of scope**
section says so, on the day it was accepted:

> A per-session detail surface (the roster's eventual right home — a hover panel
> under-serves per-child tokens, cost, outcome, and Errand)

A hover panel is the wrong instrument for a surface whose entire job is being
read: it is unreachable by touch, it cannot be held open while the eye moves
between a child's Errand and the parent's own numbers, and it disappears on the
smallest pointer drift. Three ADR updates since — #85, #87 — have made the roster
carry more truth per row (a verbatim outcome word, resume, per-child model and
cost), and every one of them lands in a panel that vanishes when you look away.

The #65 research reached the same rendering independently, from a source that has
no hierarchy of its own to copy:

> What *is* reusable is the turn fold's shape: an indented, left-bordered,
> collapsed child block under its parent … it can be a foldable child block under
> the parent's row instead, keeping Codex's no-card-of-its-own decision intact
> while making the relationship visible.

## Decision

The roster renders as **a fold under its parent's row**: indented, left-bordered,
**collapsed by default**, opened and closed by the person.

- **The Band's count keeps counting Agent Sessions**, and one Agent Session stays
  one row in the Band's run however many Sub-agents it opens. This is the
  constraint 0014's objection was protecting, stated as a rule instead of as a
  reason to draw nothing.
- **Collapsed by default**, so the Board's first paint is unchanged and no
  expansion is inflicted on someone who did not ask for one. The fold's own
  summary line carries the count while it is shut, which is what the badge said
  before.
- **The person controls it, and only the person.** Nothing in a poll, a resume,
  or a Sub-agent's ending opens or closes a fold. A surface that reorganises
  itself under the reader's eye is the failure the Board's oldest-waiting-first
  ordering already refuses.
- **One level.** The fold is root Agent Session → its Sub-agents. Depth is still
  carried and still not drawn: 5 of 135 observed Sub-agents nest at all
  (0014), and a tree drawn for 4% of rows is a tree that reads as broken for the
  other 96%.
- **It is a disclosure, not a tooltip.** It is reachable from the keyboard,
  states its open/shut state to a screen reader, and stays open until closed.

## Considered options

**Keep the hover panel and make it bigger.** Rejected: size is not what
under-serves. Hover is. Every measurement 0014 and its updates added to a roster
row — cost at the child's own model, the verbatim outcome word, resume — is
something a person compares against a sibling row or the parent's total, and
comparison is what a panel that vanishes cannot support.

**Go straight to the per-session detail surface** 0014 named as the roster's
eventual right home. Rejected as the next step, not as the destination: it is a
new surface with its own routing, addressing, and empty states, and the fold is
the smallest change that makes the hierarchy visible. The fold does not preclude
it — a detail surface would inherit the same rows.

**Promote Sub-agents to their own cards under a nested Band.** Rejected by 0014
for reasons this ADR does not touch: a Sub-agent carries no independent human
need. A nested Band would also be the one shape that genuinely breaks the count.

## Consequences

- **The Board's vertical rhythm becomes variable** for the first time. Collapsed
  by default bounds it: nothing moves until someone opens something.
- **Expansion state is per-card and is not persisted.** A reload closes every
  fold. Persisting it is a preference store this repo does not have, and an
  unpersisted fold is honest about that rather than half-remembering.
- **The badge's two states become the fold's summary line.** The running/still
  distinction 0014 established, and the accessible name that carries it, are the
  part of the badge that survives — they were never about hover.
- **The pure/rendered split holds.** `roster.ts` already states the discipline:
  the rows and the badge are observable without rendering. Whatever the fold's
  summary line says belongs there too, not in the component.
