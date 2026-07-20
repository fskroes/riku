import { useEffect, useState } from "react";
import type { LinkedSession, ProjectRef, WorkItem, WorkStatus } from "./types";
import { domId, shortModel, sourceLabel } from "./format";
import { Machine, Tile, useFlash } from "./ui";
import { useWork } from "./useWork";
import { WorkGraphPrototype, prototypeActive } from "./WorkGraphPrototype"; // PROTOTYPE gate — remove with the file

const COLUMNS: { key: WorkStatus; label: string }[] = [
  { key: "todo", label: "To do" },
  { key: "doing", label: "In progress" },
  { key: "done", label: "Done" },
];

const STATUS_GLYPH: Record<WorkStatus, string> = { todo: "○", doing: "◐", done: "✓" };

export type OpenSession = (sessionId: string) => void;

/** The project selector — a pill dropdown, one project at a time (ADR 0005: no
 *  all-projects roll-up). Projects come from the live sessions. */
function ProjectSelector({
  project,
  projects,
  onSelect,
}: {
  project: ProjectRef;
  projects: ProjectRef[];
  onSelect: (p: ProjectRef) => void;
}) {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const close = (): void => setOpen(false);
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [open]);

  return (
    <span style={{ position: "relative" }}>
      <button
        className="pill"
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        ▤ {project.name} <span className="chev">▾</span>
      </button>
      {open && (
        <div className="menu">
          {projects.map((p) => (
            <button
              key={p.cwd}
              type="button"
              aria-current={p.cwd === project.cwd}
              onClick={() => {
                setOpen(false);
                onSelect(p);
              }}
            >
              ▤ {p.name}
            </button>
          ))}
        </div>
      )}
    </span>
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

/** The dependency-graph rendering (ADR 0005): the same item set laid out left to
 *  right by blocked-by depth, with edges from each blocker to its dependents. */
function Graph({ items, onOpenSession }: { items: WorkItem[]; onOpenSession: OpenSession }) {
  const byId = new Map(items.map((i) => [i.id, i]));
  const depth = computeDepths(items);

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
  const width = (maxCol + 1) * NODE_W + maxCol * GAP_X;
  const height = maxRow * NODE_H + Math.max(0, maxRow - 1) * GAP_Y;

  const edges: { from: string; to: string }[] = [];
  for (const item of items) {
    for (const b of item.blockedBy) {
      if (byId.has(b)) edges.push({ from: b, to: item.id });
    }
  }

  if (items.length === 0) return <div className="empty">No Work Items to graph.</div>;

  return (
    <div className="graph" style={{ width, height }}>
      <svg className="edges" width={width} height={height} aria-hidden>
        {edges.map(({ from, to }) => {
          const a = pos.get(from)!;
          const b = pos.get(to)!;
          const x1 = a.x + NODE_W;
          const y1 = a.y + NODE_H / 2;
          const x2 = b.x;
          const y2 = b.y + NODE_H / 2;
          const dx = Math.max(24, (x2 - x1) / 2);
          return (
            <path
              key={`${from}->${to}`}
              d={`M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`}
              className="edge"
              fill="none"
            />
          );
        })}
      </svg>
      {items.map((item) => {
        const p = pos.get(item.id)!;
        return (
          <button
            key={item.id}
            type="button"
            className={`gnode is-${item.status}`}
            id={domId("item", item.id)}
            style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H }}
            title={item.session ? "Open the linked session on the Board" : item.title}
            onClick={() => item.session && onOpenSession(item.session.id)}
          >
            <div className="gtop">
              <span className="id">{item.id}</span>
              {item.effort && <span className="eff">{item.effort}</span>}
              {item.session && <span className="dot live" />}
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
  const { data, loading, error } = useWork(project?.cwd ?? null);

  const items = data?.items ?? [];
  // The item to reveal when arriving from a Board session's "plan" link.
  const focusItem = focusSessionId ? items.find((i) => i.session?.id === focusSessionId) : undefined;
  useFlash(focusItem ? domId("item", focusItem.id) : null);

  // PROTOTYPE — "?variant=A|B|C" replaces the graph body with the throwaway
  // work-graph variants. Works even with no project (falls back to a demo DAG).
  if (prototypeActive()) {
    return <WorkGraphPrototype liveItems={items} onOpenSession={onOpenSession} />;
  }

  if (!project) {
    return <div className="empty">No projects yet — a project appears once it has a session.</div>;
  }

  const done = items.filter((i) => i.status === "done").length;
  const total = items.length;
  const pct = total ? Math.round((done / total) * 100) : 0;

  return (
    <>
      <div className="work-head">
        <ProjectSelector project={project} projects={projects} onSelect={onSelectProject} />
        <span className="src">
          source: {data ? sourceLabel(data.source) : "…"} · {total} items
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

      {error && total === 0 ? (
        <div className="empty">Couldn’t read Work Items for this project.</div>
      ) : loading && total === 0 ? (
        <div className="empty">Loading Work Items…</div>
      ) : total === 0 ? (
        <div className="empty">No Work Items ({data ? sourceLabel(data.source) : "—"}).</div>
      ) : mode === "kanban" ? (
        <Kanban items={items} onOpenSession={onOpenSession} />
      ) : (
        <div className="graph-scroll">
          <Graph items={items} onOpenSession={onOpenSession} />
        </div>
      )}
    </>
  );
}
