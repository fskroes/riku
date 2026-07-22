# 0011 — Process liveness as status ground truth

## Status

Accepted (2026-07-22).

## Context

Since C1, Running vs Finished was a pure file-mtime heuristic (`ACTIVITY_WINDOW`,
15 minutes). It lies in both directions: a Ctrl-C'd session keeps a fresh
transcript and shows Running for up to 15 minutes, and a live agent whose user is
thinking goes quiet past the window and shows Finished. A UI prototype comparing
three policies on the real board (one-way dead→Finished, liveness-authoritative,
status quo) settled the choice.

## Decision

Observe the actual agent processes once per refresh tick and make Process
Liveness authoritative in **both** directions where a match exists:

- Alive → Active, no matter how quiet the transcript.
- Dead (debounced) → Finished, no matter how fresh the transcript.
- **Attention outranks death**: an unanswered wait stays on the Needs-attention
  band after its process exits — the card's evidence gains a factual
  "process exited" note instead of the session silently filing under Finished
  (a crashed terminal mid-approval is the session that most needs a human).
- Unknown (no `cwd`, unmatched, or probe failure) → the mtime rule, unchanged.

Mechanism (ported from a battle-tested JS implementation, macOS-only like the
rest of the project): one `ps` pass matching executable basenames
`claude`/`codex`, then a **single batched** `lsof -a -p … -d cwd -Fn` call with a
2-second budget (per-pid short timeouts fail under load). Processes in
`.claude/worktrees/agent-*` are subagent duplicates and are skipped. Sessions
match processes by canonicalized `cwd`; per directory only the newest transcript
gets the liveness credit, so a live process never resurrects historical sessions
sharing its directory. A credited session flips to Dead only after 2 consecutive
misses (anti-flap debounce), and a failed probe skips the tick entirely — it can
never mass-finish the board.

## Trade-offs

- The upstream technique's TTY filter was deliberately **dropped**: Conductor
  runs its agents with no controlling TTY (verified empirically — they show
  `??`), so the filter would have declared every Conductor session dead. Exact
  basename matching plus the worktree skip replaces it.
- An alive-but-abandoned session stays Running indefinitely rather than aging
  into Finished. This is truthful; the 24h discovery window still bounds it, and
  any "idle" treatment is a future visual concern inside the Running band, not a
  status change.
- No `pid` is recorded anywhere; `cwd` is the only bridge. Two simultaneously
  live agents in one directory collapse to one credit — accepted as rare.
