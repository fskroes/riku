/*
 * PROTOTYPE — throwaway. "What should the graph of work items look like?"
 *
 * Three structurally-different renderings of the same Work Item DAG, switchable
 * from a floating bottom bar (or ?variant=A|B|C, or ← / → keys):
 *
 *   A — Critical-Path Flow   left→right layered DAG; the longest chain is a bold
 *                            spine; hovering a node lights its whole lineage.
 *   B — Blast-Radius Orbit   foundation at the centre, dependents radiate outward
 *                            in depth rings; "what does finishing X unlock?"
 *   C — Dependency Timeline  a schedule/Gantt — items are bars placed by the
 *                            earliest stage their blockers allow.
 *
 * Mounted inside the real Work Items view (sub-shape A) so it butts against the
 * app's chrome and density. Uses live /api/work data when it carries edges,
 * else a bundled demo DAG so the graph is judgeable with no backend running.
 *
 * Cleanup: delete this file, remove the `?variant` gate in WorkItems.tsx, and
 * drop the `/* PROTOTYPE ... *​/` block at the end of styles.css.
 */
/// <reference types="vite/client" />
import { useEffect, useMemo, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent, ReactNode } from "react";
import type { LinkedSession, WorkItem, WorkStatus } from "./types";

type OpenSession = (sessionId: string) => void;
type VariantKey = "A" | "B" | "C";

const VARIANTS: { key: VariantKey; name: string }[] = [
  { key: "A", name: "Critical-Path Flow" },
  { key: "B", name: "Blast-Radius Orbit" },
  { key: "C", name: "Dependency Timeline" },
];

const GLYPH: Record<WorkStatus, string> = { todo: "○", doing: "◐", done: "✓" };

/** CSS var for a status colour. */
function statusColor(s: WorkStatus): string {
  return s === "done" ? "var(--positive)" : s === "doing" ? "var(--info)" : "var(--ink-faint)";
}

/** Effort → weight in "stages": S=1, M=2, L=3, anything else 1. */
function effortWeight(e: string | null): number {
  if (!e) return 1;
  const c = e.trim()[0]?.toUpperCase();
  return c === "L" ? 3 : c === "M" ? 2 : 1;
}

// --- Graph maths (shared) ---------------------------------------------------

interface Graph {
  items: WorkItem[];
  byId: Map<string, WorkItem>;
  depth: Map<string, number>; // longest-path column over in-set blocked-by edges
  blockers: Map<string, string[]>; // id -> in-set ids it is blocked by
  dependents: Map<string, string[]>; // id -> in-set ids that are blocked by it
  edges: { from: string; to: string }[]; // blocker -> dependent
  criticalNodes: Set<string>;
  criticalEdges: Set<string>; // "from->to"
}

function buildGraph(items: WorkItem[]): Graph {
  const byId = new Map(items.map((i) => [i.id, i]));
  const blockers = new Map<string, string[]>();
  const dependents = new Map<string, string[]>();
  for (const it of items) {
    const bs = it.blockedBy.filter((b) => byId.has(b));
    blockers.set(it.id, bs);
    for (const b of bs) dependents.set(b, [...(dependents.get(b) ?? []), it.id]);
  }
  for (const it of items) if (!dependents.has(it.id)) dependents.set(it.id, []);

  // Longest-path depth, cycle-guarded (a cycle resolves to 0).
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const d = (id: string): number => {
    const memo = depth.get(id);
    if (memo !== undefined) return memo;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const bs = blockers.get(id) ?? [];
    const val = bs.length ? 1 + Math.max(...bs.map(d)) : 0;
    visiting.delete(id);
    depth.set(id, val);
    return val;
  };
  items.forEach((i) => d(i.id));

  const edges: { from: string; to: string }[] = [];
  for (const it of items) for (const b of blockers.get(it.id) ?? []) edges.push({ from: b, to: it.id });

  // Critical path = the longest downstream chain (by node count), one representative.
  const best = new Map<string, { len: number; next: string | null }>();
  const seen = new Set<string>();
  const longest = (id: string): { len: number; next: string | null } => {
    const memo = best.get(id);
    if (memo) return memo;
    if (seen.has(id)) return { len: 1, next: null };
    seen.add(id);
    let pick: { len: number; next: string | null } = { len: 1, next: null };
    for (const dep of dependents.get(id) ?? []) {
      const sub = longest(dep);
      if (sub.len + 1 > pick.len) pick = { len: sub.len + 1, next: dep };
    }
    seen.delete(id);
    best.set(id, pick);
    return pick;
  };
  items.forEach((i) => longest(i.id));
  let start: string | null = null;
  let bestLen = 0;
  for (const it of items) {
    const l = best.get(it.id)!.len;
    if ((depth.get(it.id) ?? 0) === 0 && l > bestLen) {
      bestLen = l;
      start = it.id;
    }
  }
  const criticalNodes = new Set<string>();
  const criticalEdges = new Set<string>();
  let cur = start;
  while (cur) {
    criticalNodes.add(cur);
    const nxt = best.get(cur)!.next;
    if (nxt) criticalEdges.add(`${cur}->${nxt}`);
    cur = nxt;
  }

  return { items, byId, depth, blockers, dependents, edges, criticalNodes, criticalEdges };
}

/** All ids up-blocked-chain from `id` (its ancestors). */
function ancestorsOf(g: Graph, id: string): Set<string> {
  const out = new Set<string>();
  const walk = (x: string): void => {
    for (const b of g.blockers.get(x) ?? []) if (!out.has(b)) (out.add(b), walk(b));
  };
  walk(id);
  return out;
}
/** All ids down-dependent-chain from `id` (its descendants). */
function descendantsOf(g: Graph, id: string): Set<string> {
  const out = new Set<string>();
  const walk = (x: string): void => {
    for (const dcur of g.dependents.get(x) ?? []) if (!out.has(dcur)) (out.add(dcur), walk(dcur));
  };
  walk(id);
  return out;
}

// --- Variant A — Critical-Path Flow -----------------------------------------

const A_W = 196;
const A_H = 66;
const A_GX = 76;
const A_GY = 16;

function FlowVariant({ g, onOpenSession }: { g: Graph; onOpenSession: OpenSession }) {
  const [hover, setHover] = useState<string | null>(null);

  const layout = useMemo(() => {
    const rows = new Map<number, number>();
    const pos = new Map<string, { x: number; y: number }>();
    for (const it of g.items) {
      const col = g.depth.get(it.id) ?? 0;
      const row = rows.get(col) ?? 0;
      rows.set(col, row + 1);
      pos.set(it.id, { x: col * (A_W + A_GX), y: row * (A_H + A_GY) });
    }
    const maxCol = Math.max(0, ...[...g.depth.values()]);
    const maxRow = Math.max(0, ...[...rows.values()]);
    return {
      pos,
      width: (maxCol + 1) * A_W + maxCol * A_GX,
      height: maxRow * A_H + Math.max(0, maxRow - 1) * A_GY,
    };
  }, [g]);

  const lineage = useMemo(() => {
    if (!hover) return null;
    const set = new Set<string>([hover]);
    ancestorsOf(g, hover).forEach((x) => set.add(x));
    descendantsOf(g, hover).forEach((x) => set.add(x));
    return set;
  }, [g, hover]);

  return (
    <div className="wgp-flow" style={{ width: layout.width, height: layout.height }}>
      <svg className="wgp-edges" width={layout.width} height={layout.height} aria-hidden>
        {g.edges.map(({ from, to }) => {
          const a = layout.pos.get(from)!;
          const b = layout.pos.get(to)!;
          const x1 = a.x + A_W;
          const y1 = a.y + A_H / 2;
          const x2 = b.x;
          const y2 = b.y + A_H / 2;
          const dx = Math.max(26, (x2 - x1) / 2);
          const inLineage = lineage ? lineage.has(from) && lineage.has(to) : true;
          const critical = g.criticalEdges.has(`${from}->${to}`);
          return (
            <path
              key={`${from}->${to}`}
              d={`M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`}
              className={`wgp-edge${critical ? " crit" : ""}${inLineage ? "" : " dim"}${
                lineage && inLineage ? " lit" : ""
              }`}
              fill="none"
            />
          );
        })}
      </svg>
      {g.items.map((it) => {
        const p = layout.pos.get(it.id)!;
        const dim = lineage ? !lineage.has(it.id) : false;
        const crit = g.criticalNodes.has(it.id);
        return (
          <button
            key={it.id}
            type="button"
            className={`wgp-node is-${it.status}${crit ? " crit" : ""}${dim ? " dim" : ""}${
              it.session ? ` has-agent agent-${it.session.status}` : ""
            }`}
            style={{ left: p.x, top: p.y, width: A_W, height: A_H, borderLeftColor: statusColor(it.status) }}
            onMouseEnter={() => setHover(it.id)}
            onMouseLeave={() => setHover(null)}
            onClick={() => it.session && onOpenSession(it.session.id)}
            title={it.title}
          >
            <div className="wgp-node-top">
              <span className="glyph" style={{ color: statusColor(it.status) }}>
                {GLYPH[it.status]}
              </span>
              <span className="id">{it.id}</span>
              {crit && <span className="crit-tag">critical</span>}
              <span className="right">
                {it.session && (
                  <span className={`agent-pill ${it.session.status}`}>
                    {it.session.status === "attention"
                      ? "⚠ attention"
                      : it.session.status === "finished"
                        ? "idle"
                        : "◉ agent"}
                  </span>
                )}
                {it.effort && <span className="eff">{it.effort}</span>}
              </span>
            </div>
            <div className="wgp-node-title">{it.title}</div>
          </button>
        );
      })}
    </div>
  );
}

// --- Variant B — Blast-Radius Orbit -----------------------------------------

const B_RING = 150;
const B_MARGIN = 96;

function circMean(angles: number[]): number {
  if (!angles.length) return 0;
  let sx = 0;
  let sy = 0;
  for (const a of angles) {
    sx += Math.cos(a);
    sy += Math.sin(a);
  }
  return Math.atan2(sy / angles.length, sx / angles.length);
}

function OrbitVariant({ g, onOpenSession }: { g: Graph; onOpenSession: OpenSession }) {
  const [hover, setHover] = useState<string | null>(null);

  const layout = useMemo(() => {
    const maxDepth = Math.max(0, ...[...g.depth.values()]);
    const maxR = (maxDepth + 1) * B_RING;
    const size = 2 * (maxR + B_MARGIN);
    const cx = size / 2;
    const cy = size / 2;

    const byDepth = new Map<number, WorkItem[]>();
    for (const it of g.items) {
      const dep = g.depth.get(it.id) ?? 0;
      byDepth.set(dep, [...(byDepth.get(dep) ?? []), it]);
    }

    const angle = new Map<string, number>();
    const pos = new Map<string, { x: number; y: number }>();
    for (let dep = 0; dep <= maxDepth; dep++) {
      const ring = byDepth.get(dep) ?? [];
      if (dep === 0) {
        ring.forEach((it, i) => angle.set(it.id, (i / Math.max(1, ring.length)) * 2 * Math.PI - Math.PI / 2));
      } else {
        const target = ring.map((it) => ({
          it,
          a: circMean((g.blockers.get(it.id) ?? []).map((b) => angle.get(b)).filter((x): x is number => x != null)),
        }));
        target.sort((p, q) => p.a - q.a);
        const gap = (2 * Math.PI) / Math.max(ring.length, 5);
        let prev = -Infinity;
        for (const { it, a } of target) {
          const placed = Math.max(a, prev + gap);
          angle.set(it.id, placed);
          prev = placed;
        }
      }
      const r = (dep + 1) * B_RING;
      for (const it of ring) {
        const a = angle.get(it.id)!;
        pos.set(it.id, { x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) });
      }
    }
    return { pos, size, cx, cy };
  }, [g]);

  const lineage = useMemo(() => {
    if (!hover) return null;
    const set = new Set<string>([hover]);
    ancestorsOf(g, hover).forEach((x) => set.add(x));
    descendantsOf(g, hover).forEach((x) => set.add(x));
    return set;
  }, [g, hover]);

  return (
    <div className="wgp-orbit" style={{ width: layout.size, height: layout.size }}>
      <svg className="wgp-edges" width={layout.size} height={layout.size} aria-hidden>
        {/* faint depth rings */}
        {Array.from(new Set([...g.depth.values()])).map((dep) => (
          <circle
            key={`ring-${dep}`}
            cx={layout.cx}
            cy={layout.cy}
            r={(dep + 1) * B_RING}
            className="wgp-ring"
            fill="none"
          />
        ))}
        {g.edges.map(({ from, to }) => {
          const a = layout.pos.get(from)!;
          const b = layout.pos.get(to)!;
          const mx = (a.x + b.x) / 2;
          const my = (a.y + b.y) / 2;
          // pull the control point toward the centre so edges arc around the hub
          const cxp = mx + (layout.cx - mx) * 0.22;
          const cyp = my + (layout.cy - my) * 0.22;
          const inLineage = lineage ? lineage.has(from) && lineage.has(to) : true;
          return (
            <path
              key={`${from}->${to}`}
              d={`M ${a.x} ${a.y} Q ${cxp} ${cyp} ${b.x} ${b.y}`}
              className={`wgp-edge${inLineage ? "" : " dim"}${lineage && inLineage ? " lit" : ""}`}
              fill="none"
            />
          );
        })}
      </svg>
      <div className="wgp-hub" style={{ left: layout.cx, top: layout.cy }}>
        <span>▦</span>
        <small>foundation</small>
      </div>
      {g.items.map((it) => {
        const p = layout.pos.get(it.id)!;
        const dim = lineage ? !lineage.has(it.id) : false;
        return (
          <button
            key={it.id}
            type="button"
            className={`wgp-orb is-${it.status}${dim ? " dim" : ""}`}
            style={{ left: p.x, top: p.y }}
            onMouseEnter={() => setHover(it.id)}
            onMouseLeave={() => setHover(null)}
            onClick={() => it.session && onOpenSession(it.session.id)}
            title={`${it.id} · ${it.title}`}
          >
            <span className="orb-dot" style={{ background: statusColor(it.status) }}>
              {it.session && <span className="orb-live" />}
            </span>
            <span className="orb-body">
              <span className="id">{it.id}</span>
              <span className="orb-title">{it.title}</span>
            </span>
          </button>
        );
      })}
    </div>
  );
}

// --- Variant C — Dependency Timeline ----------------------------------------

const C_UNIT = 128; // px per stage
const C_LANE = 56;
const C_LANE_GAP = 12;
const C_PAD_TOP = 40;

function TimelineVariant({ g, onOpenSession }: { g: Graph; onOpenSession: OpenSession }) {
  const [hover, setHover] = useState<string | null>(null);

  const layout = useMemo(() => {
    // Bars ordered by earliest stage, longest first; greedy first-fit lane packing.
    const bars = g.items
      .map((it) => {
        const start = g.depth.get(it.id) ?? 0;
        const w = effortWeight(it.effort);
        return { it, start, end: start + w, w };
      })
      .sort((a, b) => a.start - b.start || b.w - a.w || a.it.id.localeCompare(b.it.id));

    const laneEnds: number[] = [];
    const lane = new Map<string, number>();
    for (const bar of bars) {
      let idx = laneEnds.findIndex((e) => e <= bar.start);
      if (idx === -1) {
        idx = laneEnds.length;
        laneEnds.push(0);
      }
      laneEnds[idx] = bar.end;
      lane.set(bar.it.id, idx);
    }

    const pos = new Map<string, { x: number; y: number; w: number }>();
    for (const bar of bars) {
      pos.set(bar.it.id, {
        x: bar.start * C_UNIT,
        y: C_PAD_TOP + lane.get(bar.it.id)! * (C_LANE + C_LANE_GAP),
        w: bar.w * C_UNIT - 14,
      });
    }
    const maxStage = Math.max(0, ...bars.map((b) => b.end));
    return {
      pos,
      width: maxStage * C_UNIT,
      height: C_PAD_TOP + laneEnds.length * (C_LANE + C_LANE_GAP),
      stages: maxStage,
    };
  }, [g]);

  const lineage = useMemo(() => {
    if (!hover) return null;
    const set = new Set<string>([hover]);
    ancestorsOf(g, hover).forEach((x) => set.add(x));
    descendantsOf(g, hover).forEach((x) => set.add(x));
    return set;
  }, [g, hover]);

  return (
    <div className="wgp-timeline" style={{ width: layout.width, height: layout.height }}>
      {/* stage gridlines + headers */}
      {Array.from({ length: layout.stages }, (_, i) => (
        <div key={`stage-${i}`} className="wgp-stage" style={{ left: i * C_UNIT, width: C_UNIT }}>
          <span className="wgp-stage-lbl">stage {i}</span>
        </div>
      ))}
      <svg className="wgp-edges" width={layout.width} height={layout.height} aria-hidden>
        {g.edges.map(({ from, to }) => {
          const a = layout.pos.get(from)!;
          const b = layout.pos.get(to)!;
          const x1 = a.x + a.w;
          const y1 = a.y + C_LANE / 2;
          const x2 = b.x;
          const y2 = b.y + C_LANE / 2;
          const dx = Math.max(20, (x2 - x1) / 2);
          const inLineage = lineage ? lineage.has(from) && lineage.has(to) : true;
          return (
            <path
              key={`${from}->${to}`}
              d={`M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`}
              className={`wgp-edge${inLineage ? "" : " dim"}${lineage && inLineage ? " lit" : ""}`}
              fill="none"
            />
          );
        })}
      </svg>
      {g.items.map((it) => {
        const p = layout.pos.get(it.id)!;
        const dim = lineage ? !lineage.has(it.id) : false;
        return (
          <button
            key={it.id}
            type="button"
            className={`wgp-bar is-${it.status}${dim ? " dim" : ""}`}
            style={{ left: p.x, top: p.y, width: Math.max(64, p.w), height: C_LANE }}
            onMouseEnter={() => setHover(it.id)}
            onMouseLeave={() => setHover(null)}
            onClick={() => it.session && onOpenSession(it.session.id)}
            title={it.title}
          >
            <span className="bar-fill" style={{ background: statusColor(it.status) }} />
            <span className="bar-body">
              <span className="bar-top">
                <span className="glyph" style={{ color: statusColor(it.status) }}>
                  {GLYPH[it.status]}
                </span>
                <span className="id">{it.id}</span>
                {it.effort && <span className="eff">{it.effort}</span>}
                {it.session && <span className="dot live" />}
              </span>
              <span className="bar-title">{it.title}</span>
            </span>
          </button>
        );
      })}
    </div>
  );
}

// --- The floating switcher --------------------------------------------------

function Switcher({ current, onChange }: { current: VariantKey; onChange: (k: VariantKey) => void }) {
  const idx = VARIANTS.findIndex((v) => v.key === current);
  const step = (delta: number): void => onChange(VARIANTS[(idx + delta + VARIANTS.length) % VARIANTS.length].key);

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "ArrowRight") step(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <div className="wgp-switch">
      <span className="wgp-switch-tag">PROTOTYPE</span>
      <button type="button" aria-label="Previous variant" onClick={() => step(-1)}>
        ←
      </button>
      <span className="wgp-switch-label">
        <b>{current}</b> — {VARIANTS[idx].name}
      </span>
      <button type="button" aria-label="Next variant" onClick={() => step(1)}>
        →
      </button>
    </div>
  );
}

// --- Demo DAG (used when live data has no edges) ----------------------------

function sess(id: string, tool: "claude" | "codex", status: LinkedSession["status"]): LinkedSession {
  return { id, project: "riku", tool, model: "claude-opus-4-8", branch: id, status, machine: "studio" };
}

const DEMO: WorkItem[] = [
  { id: "#1", title: "Design the Session data model", status: "done", effort: "S", blockedBy: [], session: null },
  { id: "#2", title: "Session source trait", status: "done", effort: "S", blockedBy: ["#1"], session: null },
  { id: "#3", title: "Claude transcript tailer", status: "done", effort: "M", blockedBy: ["#2"], session: null },
  { id: "#4", title: "Codex transcript tailer", status: "doing", effort: "M", blockedBy: ["#2"], session: sess("s-codex", "codex", "active") },
  { id: "#5", title: "Status heuristic", status: "done", effort: "S", blockedBy: ["#3"], session: null },
  { id: "#6", title: "Live git diff enrichment", status: "doing", effort: "M", blockedBy: ["#3"], session: sess("s-diff", "claude", "active") },
  { id: "#7", title: "Board axum server + SSE", status: "doing", effort: "L", blockedBy: ["#5", "#6"], session: sess("s-board", "claude", "attention") },
  { id: "#8", title: "SSE reconnect stream", status: "todo", effort: "S", blockedBy: ["#7"], session: null },
  { id: "#9", title: "React attention board UI", status: "todo", effort: "L", blockedBy: ["#7"], session: null },
  { id: "#10", title: "Work Items view", status: "todo", effort: "M", blockedBy: ["#9", "#4"], session: null },
  { id: "#11", title: "Relay wire codec", status: "done", effort: "M", blockedBy: ["#1"], session: null },
  { id: "#12", title: "Collector runtime", status: "todo", effort: "L", blockedBy: ["#11", "#4"], session: null },
  { id: "#13", title: "Homebrew release + bottles", status: "todo", effort: "S", blockedBy: ["#10", "#12"], session: null },
];

// --- Pan / scroll ------------------------------------------------------------

/** A scroll viewport that also grab-and-drag pans, so a wide graph can be moved
 *  left↔right with the mouse (native overflow still handles trackpad, scrollbar,
 *  and Shift-wheel). Dragging that starts on a node is left to the node. */
function PanScroll({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const from = useRef<{ x: number; y: number; left: number; top: number } | null>(null);
  const [panning, setPanning] = useState(false);

  const onMouseDown = (e: ReactMouseEvent): void => {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    const el = ref.current;
    if (!el) return;
    from.current = { x: e.clientX, y: e.clientY, left: el.scrollLeft, top: el.scrollTop };
    setPanning(true);
  };

  useEffect(() => {
    if (!panning) return;
    const move = (e: MouseEvent): void => {
      const f = from.current;
      const el = ref.current;
      if (!f || !el) return;
      el.scrollLeft = f.left - (e.clientX - f.x);
      el.scrollTop = f.top - (e.clientY - f.y);
    };
    const up = (): void => {
      from.current = null;
      setPanning(false);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, [panning]);

  return (
    <div ref={ref} className={`wgp-scroll${panning ? " panning" : ""}`} onMouseDown={onMouseDown}>
      {children}
    </div>
  );
}

// --- Wrapper ----------------------------------------------------------------

function readVariant(): VariantKey {
  const raw = new URLSearchParams(window.location.search).get("variant")?.toUpperCase();
  return raw === "B" || raw === "C" ? raw : "A";
}

/** True when the current URL has a ?variant param — the prototype gate. */
export function prototypeActive(): boolean {
  return !import.meta.env.PROD && new URLSearchParams(window.location.search).has("variant");
}

export function WorkGraphPrototype({
  liveItems,
  onOpenSession,
}: {
  liveItems: WorkItem[];
  onOpenSession: OpenSession;
}) {
  const [variant, setVariant] = useState<VariantKey>(readVariant);

  const change = (k: VariantKey): void => {
    setVariant(k);
    const url = new URL(window.location.href);
    url.searchParams.set("variant", k);
    window.history.replaceState(null, "", url);
  };

  const liveHasEdges = liveItems.some((i) => i.blockedBy.length > 0);
  const usingDemo = !liveHasEdges;
  const items = usingDemo ? DEMO : liveItems;
  const g = useMemo(() => buildGraph(items), [items]);

  const done = items.filter((i) => i.status === "done").length;

  return (
    <>
      <div className="wgp-head">
        <span className="wgp-title">Work graph</span>
        <span className="wgp-sub">
          {items.length} items · {done} done ·{" "}
          {usingDemo ? "demo DAG (no live dependencies found)" : "live /api/work data"}
        </span>
        {variant === "A" && (
          <span className="wgp-legend">
            <i className="lg" style={{ color: "var(--positive)" }}>✓</i> done
            <i className="lg" style={{ color: "var(--info)" }}>◐</i> doing
            <i className="lg" style={{ color: "var(--ink-faint)" }}>○</i> todo
            <i className="lg-pulse active" /> agent working
            <i className="lg-pulse attention" /> needs attention
          </span>
        )}
      </div>
      <PanScroll>
        {variant === "A" && <FlowVariant g={g} onOpenSession={onOpenSession} />}
        {variant === "B" && <OrbitVariant g={g} onOpenSession={onOpenSession} />}
        {variant === "C" && <TimelineVariant g={g} onOpenSession={onOpenSession} />}
      </PanScroll>
      <Switcher current={variant} onChange={change} />
    </>
  );
}
