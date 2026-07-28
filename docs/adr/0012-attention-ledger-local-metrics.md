# 0012 — Attention Ledger: local, append-only attention metrics

## Status

Proposed (2026-07-26).

## Context

Riku's core claim is that attention-first ordering saves a human from hunting
through terminals — but the claim is asserted, never measured. The board sorts by
Attention and oldest-`since`-first; whether that sort is *right* (do humans
actually handle the top card?) and how long agents sit blocked are both unknown.

The domain already names both ends of the measurement: **Attention Since** (when a
human need began) and **Attention Resolution** (structured evidence it was
answered, cancelled, withdrawn, or superseded). Wait time is
`resolution − since`. The `AttentionReducer` (`crates/sessions/src/attention.rs`)
is a clean choke point that already computes exactly these transitions:
`Need{key,cause,at}`, `Resolved{key}`, `Superseded`.

Three facts from the code shape any honest metric:

- **`Input` dominates by count.** Every non-`Task` tool call raises
  `Need{cause:Input}` (`session.rs:139-153`) — mechanical permission throughput,
  near-zero latency, hundreds of events to a handful of real waits. A blended
  median would read as a triumph while hiding the `Approval`/`Answer`/`Review`
  that actually cost a human minutes.
- **Resolution arrives two ways.** A *keyed* `Resolved{key}` (from a
  `tool_result` id or Codex `call_id`) means a human answered *this* need — a
  trustworthy end timestamp. `Superseded` (clean `end_turn`, a plain user turn,
  `task_complete`) means the need dissolved; its latency is censored, not
  measured. Blending them understates wait time.
- **`since` is sometimes synthetic.** When a source carries no timestamp,
  `since` is derived from file mtime (`session.rs:296-330`). Those durations are
  approximate and must be flagged, not silently averaged in.

Today Riku persists exactly one file: `config.toml`, written mode `0600`
(`crates/riku/src/config.rs`). All session state is in-memory and re-derived from
transcripts at startup; `Engine::start` rescans a 24h discovery window
(`store.rs:21,90`) and replays transcripts on every launch. Introducing a metrics
store therefore crosses a real line — Riku's first durable state beyond config —
and the replay means a naive writer double-counts yesterday's needs on every
restart. That line, and the privacy commitments Riku makes (read-only; the Relay
carries session state, never agent-control, per ADR 0002), are why this warrants
an ADR rather than a quiet commit.

A UI prototype (`web/prototype/attention-stats.html`) rendered three surfaces —
a `riku stats` terminal print (A), a full web dashboard (B), and a single-number
hero (C). A and B validated; C was rejected (the breakdowns must stay visible).
The decision was to ship the CLI surface (A) first and defer the web view (B)
until the ledger's numbers prove out on real usage.

## Decision

Add an **Attention Ledger**: a local, append-only, opt-in event log that observes
the `AttentionReducer` and records attention lifecycle transitions. It derives
nothing new — if the reducer is wrong, the ledger is wrong, which is correct,
because the ledger measures the reducer.

**Storage.** One newline-delimited JSON file at
`$XDG_DATA_HOME/riku/attention.jsonl` (else `~/.local/share/riku/attention.jsonl`),
created mode `0600` like `config.toml`. Append-only, size-capped and rotated. No
database, no schema migration engine; a version tag per record allows a future
format change.

**Records.** Two event kinds, ids and enums and timestamps only — no prose, no
Attention Evidence, no paths, no command text:

```
{v, kind:"opened",   session, key, cause, since, since_from_mtime, at}
{v, kind:"resolved", session, key, cause, since, at, resolution:"keyed"|"superseded"}
```

`session` is a stable hashed session id. `since_from_mtime` flags approximate
durations. `resolution` distinguishes a trustworthy keyed answer from a censored
supersede.

**Idempotency.** `(session, key, since)` is the dedup key. It is stable because
the reducer holds `since` across inert restatements of the same need, so replay of
the 24h discovery window on restart re-emits identical keys that the writer drops.

**Privacy and control.**

- **Opt-in, off by default** (`riku config set metrics.enabled true`).
- **Never leaves the machine** — not carried by the Collector or Relay, ever.
- **`riku stats --purge`** deletes the file in one command.

**Surface.** `riku stats` (this ADR ships the CLI only). It reads the ledger and
prints, **segmented by cause**, with `Input` reported apart from the real causes;
a **keyed vs superseded** split; **rank at resolution**; and the **unresolved
tail**. The web view (prototype variant B) is deliberately deferred.

## Trade-offs

- **Right-censoring is real and reported, not hidden.** Many attentions never
  resolve (the session ends, the machine sleeps). Dropping them biases wait time
  downward. The unresolved tail is reported as its own number, and superseded
  resolutions are kept distinct from keyed ones for the same reason.
- **Wall-clock spans nights and weekends.** A wait raised at 6pm Friday shows a
  60-hour duration that means nothing. `riku stats` reports medians and p90 (robust
  to this) and never a mean.
- **Rank needs an observer.** "Rank at resolution" is only meaningful if the board
  was open when the need resolved; a headless run has no rank. Computing rank also
  requires the ordering rule server-side — today the web client orders sessions
  (`web/src/useSessions.ts`). The rule is deterministic (Attention first, oldest
  `since` first), so it moves to / is mirrored in the backend, which makes the sort
  testable as a side benefit. Records without a live board observer omit rank.
- **First durable state.** The ledger adds rotation, corruption tolerance, and a
  data directory Riku did not have. Kept deliberately boring — one flat JSONL,
  `cat`-able in front of a security reviewer — to stay faithful to the read-only,
  local-first posture.
- **Not a score.** The ledger is diagnostic. If `resolved-from-top` is treated as
  a target to optimize, it stops measuring anything. It exists to answer whether
  the sort is right, not to grade the user.
