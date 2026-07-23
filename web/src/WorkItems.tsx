import { useEffect, useMemo, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent, ReactNode } from "react";
import type { LinkedSession, ProjectRef, WorkItem, WorkStatus } from "./types";
import { domId, shortModel, sourceLabel } from "./format";
import { Machine, Tile, useFlash } from "./ui";
import { useWork } from "./useWork";

const COLUMNS: { key: WorkStatus; label: string }[] = [
  { key: "todo", label: "To do" },
  { key: "doing", label: "In progress" },
  { key: "done", label: "Done" },
];

const STATUS_GLYPH: Record<WorkStatus, string> = { todo: "○", doing: "◐", done: "✓" };

export type OpenSession = (sessionId: string) => void;

/** The project selector — one project at a time (ADR 0005: no all-projects
 *  roll-up). A native `<select>` styled as the Paper Deck pill: keyboard
 *  traversal, type-ahead, dismissal, focus management and screen-reader semantics
 *  are the platform's, not hand-built (audit H1 / #33). The leading ▤ and trailing
 *  ▾ are decorative overlays; option text stays plain so it announces cleanly. */
function ProjectSelector({
  project,
  projects,
  onSelect,
}: {
  project: ProjectRef;
  projects: ProjectRef[];
  onSelect: (p: ProjectRef) => void;
}) {
  return (
    <span className="pill-select">
      <span className="lead" aria-hidden="true">
        ▤
      </span>
      <select
        aria-label="Project"
        value={project.cwd}
        onChange={(e) => {
          const next = projects.find((p) => p.cwd === e.target.value);
          if (next) onSelect(next);
        }}
      >
        {projects.map((p) => (
          <option key={p.cwd} value={p.cwd}>
            {p.name}
          </option>
        ))}
      </select>
      <span className="chev" aria-hidden="true">
        ▾
      </span>
    </span>
  );
}

/** A distinct, labelled state panel for the non-content states — loading failure,
 *  no project, and no Work Items (audit M1). Each carries a live-region role so a
 *  screen reader announces the change, and an optional single next action. */
function StateBlock({
  tone,
  live,
  icon,
  title,
  detail,
  action,
}: {
  tone: "error" | "info";
  live: "status" | "alert";
  icon: string;
  title: string;
  detail?: ReactNode;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className={`state-block ${tone}`} role={live}>
      <span className="ico" aria-hidden="true">
        {icon}
      </span>
      <div className="state-body">
        <b>{title}</b>
        {detail != null && <span className="detail">{detail}</span>}
      </div>
      {action && (
        <button type="button" className="state-action" onClick={action.onClick}>
          {action.label}
        </button>
      )}
    </div>
  );
}

/** The kanban structure, skeletoned, shown while the first read is in flight — the
 *  three columns are known ahead of the data, so we hold their shape rather than
 *  flashing centred "Loading…" text (audit M1/M6). */
function WorkLoading() {
  return (
    <div className="cols" role="status" aria-label="Loading Work Items…">
      {COLUMNS.map(({ key, label }) => (
        <div className="col" key={key}>
          <div className="col-head">
            <span className={`dot ${key}`} aria-hidden="true" />
            <b>{label}</b>
          </div>
          <div className="skeleton sk-wcard" aria-hidden="true" />
          <div className="skeleton sk-wcard" aria-hidden="true" />
        </div>
      ))}
    </div>
  );
}

/** The linked Agent Session chip — the Work Link, made visible. Clicking it
 *  cross-links to the same session's card on the Board. */
function SessionChip({ session, onOpen }: { session: LinkedSession; onOpen: OpenSession }) {
  const dot = session.status === "attention" ? "attention" : session.status === "finished" ? "finished" : "live";
  return (
    <button className="link-sess" type="button" title="Open this session on the Board" onClick={() => onOpen(session.id)}>
      <Tile tool={session.tool} small />
      <span className="who">
        <span className="repo">{session.project}</span>
        <span className="sub">
          {[shortModel(session.model), session.branch && `⑂ ${session.branch}`].filter(Boolean).join(" · ")}
          <Machine host={session.machine} />
        </span>
      </span>
      <span className={`dot ${dot}`} style={{ marginLeft: 2 }} />
      <span className="go">→</span>
    </button>
  );
}

/** One Work Item card in the kanban. Shows id + effort, title, and — the payload
 *  of C4 — the Work Link chip on items with a live session, or a blocked-by hint
 *  on blocked To-do items. */
function WorkCard({ item, onOpenSession }: { item: WorkItem; onOpenSession: OpenSession }) {
  const tileColor =
    item.status === "done" ? "var(--positive)" : item.status === "doing" ? "var(--info)" : "var(--ink-faint)";

  let body = null;
  if (item.session) {
    body = <SessionChip session={item.session} onOpen={onOpenSession} />;
  } else if (item.status === "todo" && item.blockedBy.length > 0) {
    body = (
      <div className="blocked">
        ⛔ Blocked ·{" "}
        {item.blockedBy.map((b) => (
          <code key={b}>{b}</code>
        ))}
      </div>
    );
  } else if (item.status === "doing") {
    body = <div className="done-line faint">◔ In progress · no live session</div>;
  } else if (item.status === "done") {
    body = <div className="done-line">✓ Done</div>;
  }

  return (
    <div className={`wcard is-${item.status}`} id={domId("item", item.id)}>
      <div className="top">
        <span className="tile sm" style={{ ["--tile" as string]: tileColor }}>
          {STATUS_GLYPH[item.status]}
        </span>
        <span className="id">{item.id}</span>
        {item.effort && <span className="eff">{item.effort}</span>}
      </div>
      <div className="title">{item.title}</div>
      {body}
    </div>
  );
}

/** The three-column To do / In progress / Done kanban (variant A layout). */
function Kanban({ items, onOpenSession }: { items: WorkItem[]; onOpenSession: OpenSession }) {
  return (
    <div className="cols">
      {COLUMNS.map(({ key, label }) => {
        const inCol = items.filter((i) => i.status === key);
        return (
          <div className="col" key={key}>
            <div className="col-head">
              <span className={`dot ${key}`} />
              <b>{label}</b>
              <span className="count">{inCol.length}</span>
            </div>
            {inCol.map((item) => (
              <WorkCard key={item.id} item={item} onOpenSession={onOpenSession} />
            ))}
          </div>
        );
      })}
    </div>
  );
}

// --- Dependency graph -------------------------------------------------------

const NODE_W = 210;
const NODE_H = 74;
const GAP_X = 60;
const GAP_Y = 18;

/** Longest-path depth (graph column) per item, over in-set blocked-by edges.
 *  Cycles resolve to depth 0 rather than looping. */
function computeDepths(items: WorkItem[]): Map<string, number> {
  const byId = new Map(items.map((i) => [i.id, i]));
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const d = (id: string): number => {
    const memo = depth.get(id);
    if (memo !== undefined) return memo;
    if (visiting.has(id)) return 0; // cycle guard
    visiting.add(id);
    const blockers = (byId.get(id)?.blockedBy ?? []).filter((b) => byId.has(b));
    const val = blockers.length ? 1 + Math.max(...blockers.map(d)) : 0;
    visiting.delete(id);
    depth.set(id, val);
    return val;
  };
  items.forEach((i) => d(i.id));
  return depth;
}

/** The accent colour for a status: green done, indigo doing, faint todo. */
function statusColor(status: WorkStatus): string {
  return status === "done" ? "var(--positive)" : status === "doing" ? "var(--info)" : "var(--ink-faint)";
}

interface GraphModel {
  edges: { from: string; to: string }[];
  blockers: Map<string, string[]>; // id -> in-set ids it is blocked by
  dependents: Map<string, string[]>; // id -> in-set ids blocked by it
  criticalNodes: Set<string>;
  criticalEdges: Set<string>; // "from->to" keys along the critical path
}

/** Blocker/dependent adjacency, the blocker→dependent edge list, and the critical
 *  path: the single longest dependency chain, so it can be drawn as a spine. Uses
 *  only in-set edges and is cycle-guarded (matches computeDepths' tolerance). */
function buildGraphModel(items: WorkItem[], depth: Map<string, number>): GraphModel {
  const byId = new Map(items.map((i) => [i.id, i]));
  const blockers = new Map<string, string[]>();
  const dependents = new Map<string, string[]>();
  for (const it of items) dependents.set(it.id, []);
  for (const it of items) {
    const bs = it.blockedBy.filter((b) => byId.has(b));
    blockers.set(it.id, bs);
    for (const b of bs) dependents.get(b)!.push(it.id);
  }
  const edges: { from: string; to: string }[] = [];
  for (const it of items) for (const b of blockers.get(it.id)!) edges.push({ from: b, to: it.id });

  // Longest downstream chain from each node (by node count), cycle-guarded.
  const best = new Map<string, { len: number; next: string | null }>();
  const visiting = new Set<string>();
  const longest = (id: string): { len: number; next: string | null } => {
    const memo = best.get(id);
    if (memo) return memo;
    if (visiting.has(id)) return { len: 1, next: null };
    visiting.add(id);
    let pick: { len: number; next: string | null } = { len: 1, next: null };
    for (const dep of dependents.get(id)!) {
      const sub = longest(dep);
      if (sub.len + 1 > pick.len) pick = { len: sub.len + 1, next: dep };
    }
    visiting.delete(id);
    best.set(id, pick);
    return pick;
  };
  items.forEach((i) => longest(i.id));

  // The critical path starts at the root (depth 0) with the longest chain.
  let start: string | null = null;
  let bestLen = 0;
  for (const it of items) {
    if ((depth.get(it.id) ?? 0) !== 0) continue;
    const len = best.get(it.id)!.len;
    if (len > bestLen) {
      bestLen = len;
      start = it.id;
    }
  }
  const criticalNodes = new Set<string>();
  const criticalEdges = new Set<string>();
  for (let cur = start; cur; cur = best.get(cur)!.next) {
    criticalNodes.add(cur);
    const nxt = best.get(cur)!.next;
    if (nxt) criticalEdges.add(`${cur}->${nxt}`);
  }
  return { edges, blockers, dependents, criticalNodes, criticalEdges };
}

/** The lineage of `id`: itself plus every ancestor (up the blocked-by chain) and
 *  every descendant (down the dependents chain) — what hovering highlights. */
function lineageOf(model: GraphModel, id: string): Set<string> {
  const set = new Set<string>([id]);
  const up = (x: string): void => {
    for (const b of model.blockers.get(x) ?? []) if (!set.has(b)) (set.add(b), up(b));
  };
  const down = (x: string): void => {
    for (const dep of model.dependents.get(x) ?? []) if (!set.has(dep)) (set.add(dep), down(dep));
  };
  up(id);
  down(id);
  return set;
}

/** The graph viewport: native overflow scroll plus grab-and-drag panning, so a
 *  wide dependency graph can be moved left↔right (and up/down) with the mouse.
 *  A drag starting on a node is left to the node — its click opens the session. */
function GraphScroll({ children }: { children: ReactNode }) {
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
    const stop = (): void => {
      from.current = null;
      setPanning(false);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", stop);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", stop);
    };
  }, [panning]);

  return (
    <div ref={ref} className={`graph-scroll${panning ? " panning" : ""}`} onMouseDown={onMouseDown}>
      {children}
    </div>
  );
}

/** Decodes the node encoding at a glance — status glyphs, the critical spine, and
 *  the live-agent rings. */
function GraphLegend() {
  return (
    <div className="graph-legend">
      <span>
        <i className="lg" style={{ color: "var(--positive)" }}>
          ✓
        </i>{" "}
        done
      </span>
      <span>
        <i className="lg" style={{ color: "var(--info)" }}>
          ◐
        </i>{" "}
        doing
      </span>
      <span>
        <i className="lg" style={{ color: "var(--ink-faint)" }}>
          ○
        </i>{" "}
        todo
      </span>
      <span>
        <i className="lg-crit" /> critical path
      </span>
      <span>
        <i className="lg-pulse active" /> agent working
      </span>
      <span>
        <i className="lg-pulse attention" /> needs attention
      </span>
    </div>
  );
}

/**
 * The dependency-graph rendering (ADR 0005): the item set laid out left to right
 * by blocked-by depth, edges from each blocker to its dependent. The longest
 * dependency chain is drawn as an amber "critical path" spine; hovering a node
 * highlights its full lineage (ancestors + descendants) and dims the rest; a node
 * an agent is live on gets a status-coloured pulsing ring (green active, amber
 * attention). Done items read at a glance via a ✓ glyph and a green wash.
 */
function Graph({ items, onOpenSession }: { items: WorkItem[]; onOpenSession: OpenSession }) {
  const [hover, setHover] = useState<string | null>(null);

  const { pos, width, height, model } = useMemo(() => {
    const depth = computeDepths(items);
    const model = buildGraphModel(items, depth);

    // Assign each item a (column, row) slot: column = depth, row = order within it.
    const rows = new Map<number, number>();
    const pos = new Map<string, { x: number; y: number }>();
    for (const item of items) {
      const col = depth.get(item.id) ?? 0;
      const row = rows.get(col) ?? 0;
      rows.set(col, row + 1);
      pos.set(item.id, { x: col * (NODE_W + GAP_X), y: row * (NODE_H + GAP_Y) });
    }
    const maxCol = Math.max(0, ...[...depth.values()]);
    const maxRow = Math.max(0, ...[...rows.values()]);
    return {
      pos,
      model,
      width: (maxCol + 1) * NODE_W + maxCol * GAP_X,
      height: maxRow * NODE_H + Math.max(0, maxRow - 1) * GAP_Y,
    };
  }, [items]);

  const lineage = useMemo(() => (hover ? lineageOf(model, hover) : null), [model, hover]);

  if (items.length === 0) return <div className="empty">No Work Items to graph.</div>;

  return (
    <div className="graph" style={{ width, height }}>
      <svg className="edges" width={width} height={height} aria-hidden>
        {model.edges.map(({ from, to }) => {
          const a = pos.get(from)!;
          const b = pos.get(to)!;
          const x1 = a.x + NODE_W;
          const y1 = a.y + NODE_H / 2;
          const x2 = b.x;
          const y2 = b.y + NODE_H / 2;
          const dx = Math.max(24, (x2 - x1) / 2);
          const crit = model.criticalEdges.has(`${from}->${to}`);
          const lit = lineage ? lineage.has(from) && lineage.has(to) : false;
          const cls = `edge${crit ? " crit" : ""}${lineage ? (lit ? " lit" : " dim") : ""}`;
          return (
            <path
              key={`${from}->${to}`}
              d={`M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`}
              className={cls}
              fill="none"
            />
          );
        })}
      </svg>
      {items.map((item) => {
        const p = pos.get(item.id)!;
        const crit = model.criticalNodes.has(item.id);
        const dim = lineage ? !lineage.has(item.id) : false;
        const agent = item.session ? ` has-agent agent-${item.session.status}` : "";
        return (
          <button
            key={item.id}
            type="button"
            className={`gnode is-${item.status}${crit ? " crit" : ""}${dim ? " dim" : ""}${agent}`}
            id={domId("item", item.id)}
            style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H }}
            title={item.session ? "Open the linked session on the Board" : item.title}
            onMouseEnter={() => setHover(item.id)}
            onMouseLeave={() => setHover(null)}
            onClick={() => item.session && onOpenSession(item.session.id)}
          >
            <div className="gtop">
              <span className="glyph" style={{ color: statusColor(item.status) }}>
                {STATUS_GLYPH[item.status]}
              </span>
              <span className="id">{item.id}</span>
              {crit && <span className="crit-tag">critical</span>}
              <span className="gright">
                {item.session && (
                  <span className={`agent-pill ${item.session.status}`}>
                    {item.session.status === "attention"
                      ? "⚠ attention"
                      : item.session.status === "finished"
                        ? "idle"
                        : "◉ agent"}
                  </span>
                )}
                {item.effort && <span className="eff">{item.effort}</span>}
              </span>
            </div>
            <div className="gtitle">{item.title}</div>
          </button>
        );
      })}
    </div>
  );
}

// --- View shell -------------------------------------------------------------

/**
 * The Work Items view for one project: a project selector, a source badge, and
 * the item set rendered two ways (kanban + dependency graph). Cross-links both
 * ways with the Board via `focusSessionId` (highlight the item a Board session
 * links to) and `onOpenSession` (jump back to that session's card).
 */
export function WorkItems({
  project,
  projects,
  onSelectProject,
  focusSessionId,
  onOpenSession,
}: {
  project: ProjectRef | null;
  projects: ProjectRef[];
  onSelectProject: (p: ProjectRef) => void;
  focusSessionId: string | null;
  onOpenSession: OpenSession;
}) {
  const [mode, setMode] = useState<"kanban" | "graph">("kanban");
  const { data, loading, error, refetch } = useWork(project?.cwd ?? null);

  const items = data?.items ?? [];
  // The item to reveal when arriving from a Board session's "plan" link.
  const focusItem = focusSessionId ? items.find((i) => i.session?.id === focusSessionId) : undefined;
  useFlash(focusItem ? domId("item", focusItem.id) : null);

  if (!project) {
    return (
      <StateBlock
        tone="info"
        live="status"
        icon="▤"
        title="No project selected yet."
        detail="A project appears here once it has an Agent Session in the last 24h."
      />
    );
  }

  const done = items.filter((i) => i.status === "done").length;
  const total = items.length;
  const pct = total ? Math.round((done / total) * 100) : 0;
  // A background refetch failed while a plan is already on screen: keep the data,
  // flag it quietly instead of blanking the view (audit M1).
  const staleRefresh = error && total > 0;
  const source = data ? sourceLabel(data.source) : null;

  return (
    <>
      <div className="work-head">
        <ProjectSelector project={project} projects={projects} onSelect={onSelectProject} />
        <span className="src">
          source: {source ?? "…"} · {total} items
        </span>
        <span className="seg small">
          <button type="button" aria-pressed={mode === "kanban"} onClick={() => setMode("kanban")}>
            Kanban
          </button>
          <button type="button" aria-pressed={mode === "graph"} onClick={() => setMode("graph")}>
            Graph
          </button>
        </span>
        <span className="prog">
          {done}/{total} done
          <span className="track">
            <span style={{ width: `${pct}%` }} />
          </span>
        </span>
      </div>

      {staleRefresh && (
        <div className="stale-refresh" role="status">
          <span className="dot" aria-hidden="true" />
          Couldn’t refresh — showing the last version.
          <button type="button" onClick={refetch}>
            Retry
          </button>
        </div>
      )}

      {error && total === 0 ? (
        <StateBlock
          tone="error"
          live="alert"
          icon="⚠"
          title="Couldn’t read Work Items for this project."
          detail="The plan source didn’t respond."
          action={{ label: "Retry", onClick: refetch }}
        />
      ) : loading && total === 0 ? (
        <WorkLoading />
      ) : total === 0 ? (
        <StateBlock
          tone="info"
          live="status"
          icon="▤"
          title={`No Work Items yet${source ? ` (${source})` : ""}.`}
          detail={
            data?.source === "workMd"
              ? "Add tasks to WORK.md, then refresh."
              : data?.source === "github"
                ? "Open issues in this repo will appear here."
                : undefined
          }
          action={{ label: "Refresh", onClick: refetch }}
        />
      ) : mode === "kanban" ? (
        <Kanban items={items} onOpenSession={onOpenSession} />
      ) : (
        <>
          <GraphLegend />
          <GraphScroll>
            <Graph items={items} onOpenSession={onOpenSession} />
          </GraphScroll>
        </>
      )}
    </>
  );
}
