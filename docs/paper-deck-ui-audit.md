# Paper Deck UI/UX audit (issue #32)

Audit of the surfaces intentionally left unchanged when the Board adopted Paper Deck.
Grouped by severity. Each finding names the owning sub-issue (#33–#37); #38 verifies
the integrated result. Findings are grounded in the current `web/src` source.

Format per finding: **problem · surface/state · smallest change · primitive
sufficient? · new dependency? · a11y/responsive acceptance**.

---

## Critical — blocks the accessibility mandate everywhere

### C1. No visible focus style is defined anywhere → keyboard users are lost (#34, cross-cutting)
- **User problem:** A keyboard-only or low-vision user cannot see where focus is. `styles.css` defines **zero** `:focus-visible` rules, so every custom button (rail nav, pill, menu items, segmented toggle, plan/open links, session chips, graph nodes) falls back to the UA outline — which is invisible on transparent backgrounds and is **clipped by `overflow: hidden`** on `.gnode` and `.wcard`/`.link-sess` regions.
- **Surface/state:** Every interactive surface, focused state.
- **Smallest change:** One global rule — `:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: inherit; }` — plus removing `overflow:hidden` where it clips focus (or using `box-shadow` rings there).
- **Primitive sufficient:** Yes — pure CSS.
- **New dependency:** No.
- **Acceptance:** Every interactive element shows a ≥2px, ≥3:1-contrast focus indicator not clipped by its container, at desktop and narrow widths.

---

## High — a stated acceptance criterion currently fails

### H1. Hand-built project dropdown has real focus/keyboard/dismissal gaps (#33)
- **User problem:** `ProjectSelector` (`WorkItems.tsx:20`) opens on click and closes on any `document click`. It has **no `aria-expanded`/`aria-haspopup`, no `role` on the menu, no arrow-key navigation, no Escape-to-close, no focus move into the menu, and no focus return to the trigger.** Keyboard and screen-reader users cannot operate it as a menu.
- **Surface/state:** Work Items project selector, open/closed/keyboard.
- **Smallest change:** Replace the hand-built open/close with an accessible listbox/menu primitive.
- **Primitive sufficient:** No — the gaps #33 anticipated are confirmed. This is the one place a dependency is justified.
- **New dependency:** **Yes — Base UI** (the curated choice named in #33) for its Menu/Select, *if* no primitive already in `package.json` covers it. Verify existing deps first.
- **Acceptance:** Open/close and option selection by keyboard; Escape closes and returns focus; `aria-expanded` on trigger; options announced with selected state; works at narrow width without losing the Board cross-link.

### H2. Rail nav uses contradictory tab semantics (#34)
- **User problem:** The rail (`App.tsx:125`) is `role="tablist"` with `role="tab"` buttons carrying **`aria-pressed`** (a toggle-button attribute, not a tab attribute). Tabs should use `aria-selected`, own their panel via `aria-controls`, expose a `role="tabpanel"` on `<main>`, and support arrow-key traversal — none of which exist. Screen readers announce a broken tab widget.
- **Surface/state:** Application frame, Board/Work Items switch.
- **Smallest change:** Either make it a real tablist (`aria-selected`, `aria-controls`, `tabpanel`, arrow keys) **or** drop to `<nav>` with `aria-current="page"` on the active button. The latter is smaller and fits a two-destination switch.
- **Primitive sufficient:** Yes — semantics + a small key handler if kept as tabs.
- **New dependency:** No.
- **Acceptance:** Correct role/selected semantics, visible focus, keyboard order matches visual order at desktop and narrow widths.

### H3. Rail status indicators are color-only, and the stream stat has no text at all (#34)
- **User problem:** Live/Relay/stream state is conveyed by colored dots + `title` tooltips. The **stream stat (`App.tsx:138`) renders only a dot — no number, no text** — so it is invisible to screen readers and unreadable to color-blind users; `title` is not exposed to keyboard or touch.
- **Surface/state:** Rail foot, connected/reconnecting/solo states.
- **Smallest change:** Add `aria-label` (or visually-hidden text) to each stat, and a non-color cue (glyph/shape or short text) for connected vs reconnecting vs solo.
- **Primitive sufficient:** Yes.
- **New dependency:** No.
- **Acceptance:** Relay/stream/machine/live-session/cost states distinguishable without color and each has an accessible name.

### H4. Dependency graph exposes structure only visually (#37)
- **User problem:** Edges are `aria-hidden` SVG; "blocked by" / "blocks" is conveyed purely by position and curves; the critical path by amber stroke. A node button's `title` is only its title or "Open the linked session." A non-visual user gets **no dependency information at all**. Lineage highlight is `onMouseEnter/Leave` only — **no focus/keyboard equivalent** — and there is **no small-screen ordered fallback** (fixed 210px nodes in a pan-scroll region).
- **Surface/state:** Work Items graph, keyboard/SR/narrow-screen.
- **Smallest change:** Add per-node accessible text ("Blocked by A, B; blocks C") and expose blocked-by relations semantically; trigger lineage on focus as well as hover; provide an ordered dependency list as the narrow-screen presentation (the depth ordering already exists in `computeDepths`).
- **Primitive sufficient:** Yes — existing model computes depth/critical path; needs semantic surfacing, not a graph library.
- **New dependency:** No.
- **Acceptance:** Nodes + links keyboard-reachable with visible focus; dependencies/status/critical-path/agent-presence exposed without relying only on color/position/animation; narrow screens get a usable ordered view; legend explains every remaining encoding.

---

## Medium — degrades the "one product" feel or a specific state

### M1. Loading / error / empty / no-project states are visually identical (#33)
- **Problem:** All four render the same `.empty` centered text (`WorkItems.tsx:480–516`), differing only in words, with **no `role="status"`/`aria-live`** and **no next action** (no retry on error, no action on empty). A refetch failure while data is on screen shows **no indication at all** (`useWork.ts:38` keeps stale data silently).
- **Surface/state:** Work Items, loading/error/no-project/no-items.
- **Smallest change:** Distinct treatments per state; wrap transient states in `aria-live="polite"`; add a Retry action to error and a useful action to empty; surface a quiet "couldn't refresh" cue when a background refetch fails with stale data shown.
- **Primitive sufficient:** Yes. **New dependency:** No.
- **Acceptance:** The four states are visually + semantically distinct; every empty state offers one useful action when available; source identity (`WORK.md` vs GitHub) stays visible.

### M2. Truncation with no accessible disclosure (#35)
- **Problem:** `.branch`, `.act`, `.rbranch` ellipsis-truncate and `.evidence` line-clamps with **no `title` and no expand**, so sighted pointer/keyboard users can't recover clamped text. `open failed` and machine host hide their value in `title` only.
- **Surface/state:** All session presentations, long content.
- **Smallest change:** Add `title` to truncated single-line fields; make clamped evidence expandable or `title`-backed; give the "open failed" message a visible/expandable form, not `title`-only.
- **Primitive sufficient:** Yes. **New dependency:** No.
- **Acceptance:** Long project/branch/activity/evidence truncate predictably and have an accessible way to reveal full text at narrow width and zoom.

### M3. Queued Attention cards silently drop the machine chip (#35)
- **Problem:** `.focus-board > aside .alert .routing { display: none }` (`styles.css:286`) hides the machine chip for **queued** alerts, so the same card exposes fewer facts in the queue than as the primary — a hierarchy inconsistency, and local/remote context is lost for queued needs.
- **Surface/state:** "Up next" queue.
- **Smallest change:** Keep machine context in the queue (compact form) rather than removing it.
- **Primitive sufficient:** Yes. **New dependency:** No.
- **Acceptance:** Primary/queued/Running/Finished expose the same facts with an intentional per-context hierarchy; local vs remote stays distinguishable.

### M4. Symbol/emoji glyphs read as noise to screen readers (#35, #34)
- **Problem:** `⑂ branch`, `↑{in}/{out}` tokens, `+/−` diff, `▤ plan ↗`, `open ↗`, status glyphs `○ ◐ ✓` carry meaning through characters a screen reader reads literally (e.g. token counts announce as "up-arrow 3k slash 1k"). No labels on the token pair or diff.
- **Surface/state:** Every card, SR.
- **Smallest change:** Add `aria-label`s ("tokens in/out", "lines added/removed", "branch", "status: done") and mark purely decorative glyphs `aria-hidden`.
- **Primitive sufficient:** Yes. **New dependency:** No.
- **Acceptance:** Stats, status dots/glyphs, tabs, progress, and icon-only actions have screen-reader labels.

### M5. Low-contrast faint ink fails WCAG for meaningful text (cross-cutting, verify in #38)
- **Problem:** `--ink-faint: rgba(43,36,26,0.38)` on `--bg-content #FAF6EF` is well below 4.5:1 (and likely below 3:1). It's used for real content — `.age`, `.eff`, todo dots/glyphs, the `est.` label, `.done-line.faint`, graph todo nodes.
- **Surface/state:** All views, default.
- **Smallest change:** Darken the faint token (or restrict it to genuinely non-essential decoration) and re-check `--ink-muted` (0.62) for small text.
- **Primitive sufficient:** Yes. **New dependency:** No.
- **Acceptance:** Text and meaningful glyphs meet WCAG AA contrast at default zoom.

### M6. Board and stream have no loading/reconnecting feedback (#34; verify in #38)
- **Problem:** The Board conflates loading with empty — "No agent sessions in the last 24h" (`Board.tsx:195`) shows immediately before the first data arrives. Stream reconnection is only the color-only rail dot (H3); there is no inline banner when the local event stream or Relay drops.
- **Surface/state:** Board first paint; stream/Relay reconnecting.
- **Smallest change:** Distinguish "connecting…" from "empty" on first load; add an unobtrusive reconnecting indicator with an accessible name.
- **Primitive sufficient:** Yes (skeleton where structure is known — the focus-board shell — is cheap CSS). **New dependency:** No.
- **Acceptance:** Loading, empty, and reconnecting are represented distinctly for the local stream and Relay.

---

## Low — polish; batch into the owning ticket

- **L1 (#34):** Cost toggle is a `$` button with `aria-pressed` but no accessible name beyond the glyph — add `aria-label`. Brand `▦` is a `title`-only span — give it a label or mark decorative.
- **L2 (#37):** Panning is `mousedown`/`mousemove` only — no touch/pointer events and no keyboard scroll of the viewport; native overflow scroll partly covers touch but drag-pan won't work on touch. The global reduced-motion rule stops the graph pulse (status survives via the persistent `outline` + agent pill — good), but the legend still says "pulse"; reconcile the legend wording under reduced motion.
- **L3 (#36):** Kanban columns are plain `div`s (no list semantics); empty columns show only a `0` count with no "nothing here" cue; the progress `.track` has no `role="progressbar"`/aria values (the adjacent `done/total` text does carry the value, so this is minor).
- **L4 (#37):** Graph nodes without a linked session are focusable buttons whose click does nothing (`Graph` `onClick` guards on `item.session`) — either make them non-interactive or give them a purpose (e.g. focus lineage).
- **L5 (#38, evidence-gated):** No virtualization anywhere; kanban and graph render all nodes. Do not add a virtualization dependency without measuring realistic list sizes first, per #38.

---

## Dependency summary

Only **one** finding (H1, the project dropdown) plausibly justifies a new dependency —
**Base UI**, and only after confirming no existing `package.json` primitive covers a
listbox/menu. **Every other finding is CSS + semantics + small handlers** using what's
already here. This matches #32's "existing primitives first, dependency only with
evidence" constraint.

## Suggested sequencing

1. **C1 first** (global focus-visible) — unblocks the a11y acceptance on every ticket.
2. **#33** (keystone: H1, M1) — gates #36 and #37.
3. **#34** (H2, H3, M6, L1) and **#35** (M2, M3, M4) in parallel — both unblocked.
4. **#36** (L3) and **#37** (H4, L2, L4) after #33.
5. **M5 contrast** as a shared token fix, verified in **#38** alongside zoom/overflow/long-list.

All findings are prototype-then-decide per #32; record accepted changes in ADR-0005
(Board/Work Items) and note the dropdown-dependency decision there before implementing.
