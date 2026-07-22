# Collector + Relay: the team / multi-machine board

**Partially superseded by ADR 0010:** the target architecture no longer reuses the
browser-domain `Session` as the Collector–Relay wire model, and the shared Attention
band is labelled **Needs attention**, not “needs you.” The topology, streaming
framing, machine identity, ephemeral Relay, merge behavior, authentication, and
read-only boundary below remain in effect. ADR 0010 is accepted but not yet
implemented, so passages describing the current shared wire shape remain as
historical implementation context.

C7 turns the single-machine board into a fleet view: sessions running on other
machines appear as ordinary cards, each labelled with its machine. It adds the two
remaining pieces from the settled architecture — a **Collector** (headless, runs on
any machine) and a **Relay** (a hub, run once anywhere reachable) — without changing
anything about zero-setup solo use.

**`sessions::Event` is the one wire currency, in both directions.** The
Collector→Relay push and the Relay→Board fan-out both carry the existing `Event`
(`Upsert(Session)` / `Removed { id }`). Because every `Upsert` is a full Session
snapshot, both streams are idempotent and self-healing: a dropped frame or a
reconnect re-syncs on the next snapshot, exactly like the board's local
`/api/events`. `Event` and `Session` gained `Deserialize` (they already serialized
camelCase for the UI), so the same shape the board serves to a browser is the shape
that crosses the network — no second model, no translation layer. `machine` and
`diff` are `#[serde(default)]` so the wire stays additive in both directions.

**Two framings, chosen per hop; both JSON `Event`.** Collector→Relay is a long-lived
streaming `POST /collect` of newline-delimited `Event`s: a persistent connection is
what lets the Relay detect a Collector going offline (User Story 7). Relay→Board is
SSE (`GET /subscribe`), mirroring the board's own `/api/events` so the "snapshot then
live stream" contract holds end to end; the board is a Rust subscriber, so it decodes
the `Event` straight out of each `data:` field rather than using named-event framing.
A shared `wire` module owns the codec and the token check, so the Relay, the
Collector, and the board's subscription client never diverge.

**Machine identity is stamped at the source, never derived by the Relay.** The
Collector (and the board's own local runtime, since step 1) sets `Session.machine`
to the host's name before an `Event` leaves the watcher. The Relay only relays
already-tagged `Event`s — it never infers identity from a network address or a
client-supplied path — so a board running both local sources and a Relay subscription
labels every card consistently, with no unlabelled "local" special case (User Story
23). The Collector reuses the board's whole pipeline (`SessionStore` over the same
Session Sources, the watcher, the 30s refresh, and live git `+/-` enrichment, now
shared as `sessions::DiffCache`), so a remote card carries the same stats a local
one does (User Story 18). The Collector fills `diff` itself because the repo lives on
its machine — the board cannot read a working tree it does not have.

**The Relay is the merge point, in memory only (ADR 0004).** It keeps a map of
session id → latest Session across all connected Collectors, serves a new subscriber
that snapshot then a live stream, and holds nothing durable — a restart loses
nothing because Collectors re-push. Each Collector connection carries a generation id
so the disconnect-reaping is race-safe: when a connection drops, the Relay emits
`Removed` only for sessions it still owns, so a Collector that reconnects on a fresh
connection before the old one's cleanup runs is not clobbered.

**The board merges remote sessions at the output boundary.** Relayed sessions live in
a `remote` map beside the file-backed local `SessionStore` (they have no local
transcript to tail). `/api/sessions` unions the two so a late-connecting browser sees
the whole fleet from its first fetch, and each relayed `Event` is also forwarded onto
the board's own event stream beside local Engine events, so a live browser streams
remote and local cards down one identical UI path without putting Relay events inside
the local-session Engine. On each fresh Relay connection the board resets its remote
view (clearing stale cards) and lets the incoming snapshot rebuild it, so a Relay
restart never strands a card for a machine that has since gone.

**One shared token, presented as `Authorization: Bearer …`, is the whole auth model.**
It gates both roles; a wrong or missing token is rejected before any state is
exchanged (User Stories 10/11). No accounts, no database, no token-rotation UI. The
Relay binds a configurable address (it is the intentional network service); the board
still binds localhost only. Address and token come from flags/environment, matching
the board's existing `--port`/`--root` style — no config file.

**One-way, read-only, forever (ADR 0002).** No endpoint on the Relay or the Collector
accepts a command directed at a session. The board's UI gains one atom — a machine
chip on every card (variant A from the prototype) and a topbar Relay pill
(`local only` / `relay ✓ · N machines` / reconnecting) — and nothing else: the C3
attention-first stream stays structurally unchanged, one global "needs you" ranking
across every machine.

**Workspace shape.** A new `relay` crate houses the Relay server, the Collector loop,
and the board's subscription client (all three share the wire codec), exposing the
`relay` and `collector` binaries; it depends on the shared `sessions` library, and
the `board` binary depends on it for the subscription client. TLS termination, process
supervision, and where the Relay is hosted are operator concerns, out of scope here.

Complements ADR 0001–0006; supersedes nothing.
