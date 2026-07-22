# Card stats: live git `+/-` and a labelled cost estimate

**Refined by ADR 0010:** these stats remain part of Session state and ordinary
session rows, but the default Attention row omits diff, cost, tokens, and model so
the current human need and its evidence dominate the triage surface.

C5 adds two stats to every session card: a git diff `+/-` and an estimated cost.

**Diff is live repo state, filled by the board, not the collector.** A session's
`+/-` is not in its transcript — it is the current working tree — so the collector
leaves `Session.diff` `None` and the **board** overlays it, the same seam already
used for Work Links (collector owns transcript-derived data; the board overlays
live state). The board shells out to `git` (mirroring how Work Items shell out to
`gh`) through a per-directory TTL cache, and enriches only at the output boundary
(the snapshot response and each outgoing SSE upsert) — never writing it back into
the session store, so it cannot perturb the store's change-detection. Consequence
accepted: a diff that changes while the transcript is quiet is not pushed on its
own; it refreshes on the next transcript event or the 30s status refresh. For an
active agent the transcript changes constantly (every tool call), which is exactly
when the diff changes, so this is not felt in practice.

**Diff semantics = branch work + uncommitted.** `diff_stat` measures the merge-base
of the repo's default branch → working tree, so a feature branch reports its whole
change set (commits since it forked, plus uncommitted edits) rather than only what
is uncommitted. On the default branch, or when no base resolves, it falls back to
uncommitted-vs-`HEAD`. Everything degrades to "no diff" (never an error) when the
cwd is not a git repo or `git` is unavailable.

**Cost is a labelled estimate, computed in the collector, hidden via a UI toggle.**
The figure is `tokens × the model's public list price`, matched by model-family
substring (ids drift far faster than family pricing) and returning nothing for an
unpriced model rather than guessing. It is always shown with an "est." label. We do
**not** try to detect per-session billing mode (subscription vs API) — transcripts
do not record it reliably. Instead a header toggle (`$ est. on/off`, persisted in
`localStorage`) lets a subscription user, who pays no marginal per-token cost, hide
the estimate for good. This keeps the number honest: an API-list estimate shown to
someone on a Max plan would mislead, and a manual toggle is the truthful
affordance. Rejected alternatives: a server `--subscription` flag (needs a restart,
less flexible) and per-session detection (not derivable from the transcript).

`Session` drops `Eq` (keeps `PartialEq`) because `cost_usd` is an `f64`; cost is
deterministic from tokens + model, so equal projections stay equal and the store's
`!=` change-detection does not churn.

Complements ADR 0001–0005; supersedes nothing.
