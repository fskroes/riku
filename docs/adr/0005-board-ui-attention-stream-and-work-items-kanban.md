# Board UI: attention-first stream + per-project Work Items kanban

**Refined by ADR 0010:** the attention-first stream remains, but Explainable
Attention changes its row content, oldest-waiting-first order, shared-board label,
and clearing semantics. This ADR remains authoritative for choosing a stream over
status columns and for the Work Items view.

The board's primary view is an **attention-first focus board**, not status columns. The oldest Attention is the primary decision, later needs form an "Up next" queue beside it, Running Agent Sessions use compact two-tier rows below the primary decision, and Finished sessions dim below the queue. This structure was validated in the Paper Deck element prototype as variant C with variant B's Running-card treatment. Rejected: the three-column Active/Attention/Finished kanban (prototyped as variant A) because columns do not scale past a screenful and give Attention no more weight than Active; and a project-grouped grid because it buries "what needs me now" under project structure. The focus board keeps the human's one real question — "what needs me now?" — answerable at a glance while making the oldest need unmistakably primary.

Work Items get their own view, **one project at a time** (no all-projects roll-up), rendered two ways over the same item set: a **To do / In progress / Done kanban** (reusing variant A's column layout) and a **dependency graph** of those items (the C1→C2/C3→C7-style chains from the Work Map). A project selector switches projects and a source badge shows whether the items came from `WORK.md` or GitHub Issues. Each In-progress Work Item shows the Agent Session working it (the Work Link) as an inset chip, and that chip **cross-links both ways** with the session's card on the Board so a human moves between "the plan" and "the live work" without losing place. Cost accepted: two renderings of one Work Item set (kanban + graph) and bidirectional link state to keep in sync.

Prototype: `web/prototype/board.html` (variants A/B/C, then the converged Board + Work Items). Throwaway; fold the validated structure into `web/` during C1 (#2, the Board) and C4 (Work Items). Supersedes nothing; complements the local-first data decisions in ADR 0001–0004.
