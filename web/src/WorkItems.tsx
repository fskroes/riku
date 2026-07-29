import { useEffect, useMemo, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";
import type { LinkedSession, ProjectRef, WorkItem, WorkSource, WorkStatus } from "./types";
import { columnLabel, domId, shortModel, sourceLabel, sourceStatusNote } from "./format";
import { Branch, Machine, Tile, useFlash } from "./ui";
import { useWork } from "./useWork";
import {
  agentAccessibleText,
  blockedByLabel,
  blocksLabel,
  buildGraphModel,
  computeDepths,
  lineageOf,
  nodeAccessibleName,
  readingOrder,
  statusLabel,
} from "./graph";

const COLUMNS: { key: WorkStatus; label: string }[] = (
  ["todo", "doing", "done"] as const
).map((key) => ({ key, label: columnLabel(key) }));

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
const SESSION_STATE_LABEL: Record<LinkedSession["status"], string> = {
  attention: "needs attention",
  finished: "finished",
  active: "running",
};

function SessionChip({ session, onOpen }: { session: LinkedSession; onOpen: OpenSession }) {
  const dot = session.status === "attention" ? "attention" : session.status === "finished" ? "finished" : "live";
  const model = shortModel(session.model);
  return (
    <button className="link-sess" type="button" title="Open this session on the Board" onClick={() => onOpen(session.id)}>
      <Tile tool={session.tool} small />
      <span className="who">
        <span className="repo" title={session.project}>
          {session.project}
        </span>
        <span className="sub">
          {model && <span>{model}</span>}
          <Branch branch={session.branch} />
          <Machine host={session.machine} />
        </span>
      </span>
      {/* The status dot is colour-only decoration; its meaning is carried as
          screen-reader text so the chip's state is not glyph/colour-only (audit M4). */}
      <span className="sr-only">status: {SESSION_STATE_LABEL[session.status]}</span>
      <span className={`dot ${dot}`} style={{ marginLeft: 2 }} aria-hidden="true" />
      <span className="go" aria-hidden="true">
        →
      </span>
    </button>
  );
}

/** One Work Item card in the kanban. Shows id + effort, title, and — the payload
 *  of C4 — the Work Link chip on items with a live session, or a blocked-by hint
 *  on blocked To-do items. When a live Work Link raised the item's status (#66),
 *  it also discloses what the source itself still says, so the derived column and
 *  the plan never silently disagree. */
function WorkCard({
  item,
  source,
  onOpenSession,
}: {
  item: WorkItem;
  source: WorkSource | null;
  onOpenSession: OpenSession;
}) {
  const tileColor =
    item.status === "done" ? "var(--positive)" : item.status === "doing" ? "var(--info)" : "var(--ink-faint)";
  const derived = sourceStatusNote(item.status, item.sourceStatus, source);

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
      {derived && (
        <div className="srcstatus">
          <span aria-hidden="true">{STATUS_GLYPH[item.sourceStatus]} </span>
          {derived}
        </div>
      )}
    </div>
  );
}

/** The three-column To do / In progress / Done kanban (variant A layout). Each
 *  column is a labelled list so a screen reader announces the status, its item
 *  count, and each Work Item as a list entry; empty columns say so rather than
 *  showing a bare `0` (audit L3). `role="list"` is kept explicit because
 *  `list-style:none` drops list semantics in some browsers. */
function Kanban({
  items,
  source,
  onOpenSession,
}: {
  items: WorkItem[];
  source: WorkSource | null;
  onOpenSession: OpenSession;
}) {
  return (
    <div className="cols">
      {COLUMNS.map(({ key, label }) => {
        const inCol = items.filter((i) => i.status === key);
        const countLabel = `${label}, ${inCol.length} ${inCol.length === 1 ? "item" : "items"}`;
        return (
          <div className="col" key={key}>
            <div className="col-head">
              <span className={`dot ${key}`} aria-hidden="true" />
              <b>{label}</b>
              <span className="count">{inCol.length}</span>
            </div>
            <ul className="col-list" role="list" aria-label={countLabel}>
              {inCol.length === 0 ? (
                <li className="col-empty">Nothing here yet.</li>
              ) : (
                inCol.map((item) => (
                  <li key={item.id}>
                    <WorkCard item={item} source={source} onOpenSession={onOpenSession} />
                  </li>
                ))
              )}
            </ul>
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

/** The accent colour for a status: green done, indigo doing, faint todo. */
function statusColor(status: WorkStatus): string {
  return status === "done" ? "var(--positive)" : status === "doing" ? "var(--info)" : "var(--ink-faint)";
}

/** The graph viewport: native overflow scroll plus grab-and-drag panning, so a
 *  wide dependency graph can be moved left↔right (and up/down). Uses Pointer
 *  Events so a mouse or pen can drag-pan; touch keeps the browser's native
 *  momentum scroll (we don't capture it). A drag starting on a node is left to the
 *  node — its click opens the session. Keyboard users pan implicitly: focusing a
 *  node scrolls it into view (see Graph), so the whole graph is reachable by Tab
 *  without a drag (audit L2). */
function GraphScroll({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const from = useRef<{ x: number; y: number; left: number; top: number } | null>(null);
  const [panning, setPanning] = useState(false);

  const onPointerDown = (e: ReactPointerEvent): void => {
    // Mouse/pen only: leave touch to native scrolling so it isn't hijacked.
    if (e.pointerType === "touch" || e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    const el = ref.current;
    if (!el) return;
    from.current = { x: e.clientX, y: e.clientY, left: el.scrollLeft, top: el.scrollTop };
    setPanning(true);
  };

  useEffect(() => {
    if (!panning) return;
    const move = (e: PointerEvent): void => {
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
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
  }, [panning]);

  return (
    <div
      ref={ref}
      className={`graph-scroll${panning ? " panning" : ""}`}
      onPointerDown={onPointerDown}
      role="group"
      aria-label="Dependency graph, drag to pan"
    >
      {children}
    </div>
  );
}

/** The agent-pill wording for a linked session's status, shared by the graph node
 *  and the ordered reading so both name the state the same way. */
function agentPillText(status: LinkedSession["status"]): string {
  return status === "attention" ? "⚠ attention" : status === "finished" ? "idle" : "◉ agent";
}

/** Decodes every encoding the graph still relies on — status glyphs, the critical
 *  spine, and the live-agent rings — so nothing is colour/position-only without a
 *  key (audit H4/L2). The ring wording is reduced-motion-safe: it names what each
 *  ring *means*, not that it pulses, so it stays accurate when motion is off. */
function GraphLegend() {
  return (
    <div className="graph-legend" aria-label="Graph legend">
      <span>
        <i className="lg" style={{ color: "var(--positive)" }} aria-hidden="true">
          ✓
        </i>{" "}
        done
      </span>
      <span>
        <i className="lg" style={{ color: "var(--info)" }} aria-hidden="true">
          ◐
        </i>{" "}
        doing
      </span>
      <span>
        <i className="lg" style={{ color: "var(--ink-faint)" }} aria-hidden="true">
          ○
        </i>{" "}
        to do
      </span>
      <span>
        <i className="lg-crit" aria-hidden="true" /> critical path
      </span>
      <span>
        <i className="lg-pulse active" aria-hidden="true" /> agent working
      </span>
      <span>
        <i className="lg-pulse attention" aria-hidden="true" /> needs attention
      </span>
      <span>
        <i className="lg-pulse finished" aria-hidden="true" /> agent idle
      </span>
    </div>
  );
}

/**
 * The dependency-graph rendering (ADR 0005): the item set laid out left to right
 * by blocked-by depth, edges from each blocker to its dependent. The longest
 * dependency chain is drawn as an amber "critical path" spine; hovering *or
 * focusing* a node highlights its full lineage (ancestors + descendants) and dims
 * the rest; a node an agent is live on gets a status-coloured pulsing ring (green
 * active, amber attention). Done items read at a glance via a ✓ glyph and a green
 * wash.
 *
 * Accessibility (#37): nodes render in dependency reading order so Tab meets a
 * blocker before the item it blocks; each node's `aria-label` states its status,
 * critical-path membership, and blocked-by / blocks relations in words (edges are
 * decorative `aria-hidden` SVG); focusing a node reveals its lineage and scrolls
 * it into view, so a keyboard user can traverse and pan without a drag. The
 * narrow-screen equivalent is `GraphReading`.
 */
function Graph({ items, onOpenSession }: { items: WorkItem[]; onOpenSession: OpenSession }) {
  const [active, setActive] = useState<string | null>(null);

  const { pos, ordered, width, height, model } = useMemo(() => {
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
      ordered: readingOrder(items, depth),
      width: (maxCol + 1) * NODE_W + maxCol * GAP_X,
      height: maxRow * NODE_H + Math.max(0, maxRow - 1) * GAP_Y,
    };
  }, [items]);

  const lineage = useMemo(() => (active ? lineageOf(model, active) : null), [model, active]);

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
      {ordered.map((item) => {
        const p = pos.get(item.id)!;
        const crit = model.criticalNodes.has(item.id);
        const dim = lineage ? !lineage.has(item.id) : false;
        const agent = item.session ? ` has-agent agent-${item.session.status}` : "";
        // A node with a linked session is a button that opens it; a node without
        // one is a focusable region whose purpose is to reveal its lineage on
        // focus (audit L4 — no dead click, and the same button/div split as the
        // narrow GraphReading). Both stay keyboard-reachable so a Tab user can read
        // every node's dependency relations from its aria-label.
        const Node = item.session ? "button" : "div";
        return (
          <Node
            key={item.id}
            className={`gnode is-${item.status}${crit ? " crit" : ""}${dim ? " dim" : ""}${agent}`}
            id={domId("item", item.id)}
            style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H }}
            aria-label={nodeAccessibleName(item, model)}
            title={item.title}
            onMouseEnter={() => setActive(item.id)}
            onMouseLeave={() => setActive((a) => (a === item.id ? null : a))}
            onFocus={(e) => {
              setActive(item.id);
              // Keyboard pan: bring the focused node into the scroll viewport.
              e.currentTarget.scrollIntoView({ block: "nearest", inline: "nearest" });
            }}
            onBlur={() => setActive((a) => (a === item.id ? null : a))}
            {...(item.session
              ? { type: "button" as const, onClick: () => onOpenSession(item.session!.id) }
              : { tabIndex: 0 })}
          >
            <div className="gtop">
              <span className="glyph" aria-hidden="true" style={{ color: statusColor(item.status) }}>
                {STATUS_GLYPH[item.status]}
              </span>
              <span className="id">{item.id}</span>
              {crit && (
                <span className="crit-tag" aria-hidden="true">
                  critical
                </span>
              )}
              <span className="gright" aria-hidden="true">
                {item.session && (
                  <span className={`agent-pill ${item.session.status}`}>{agentPillText(item.session.status)}</span>
                )}
                {item.effort && <span className="eff">{item.effort}</span>}
              </span>
            </div>
            <div className="gtitle">{item.title}</div>
          </Node>
        );
      })}
    </div>
  );
}

/**
 * The narrow-screen equivalent of the dependency graph (audit H4): the same items
 * in dependency reading order (blockers before what they block) as a real ordered
 * list, so the structure a wide screen shows spatially is available where the
 * pan-scroll graph doesn't fit. Each row names its status, critical-path
 * membership, and blocked-by / blocks relations; a row with a linked session opens
 * it on the Board, matching the graph node's action.
 */
function GraphReading({ items, onOpenSession }: { items: WorkItem[]; onOpenSession: OpenSession }) {
  const { ordered, model } = useMemo(() => {
    const depth = computeDepths(items);
    return { ordered: readingOrder(items, depth), model: buildGraphModel(items, depth) };
  }, [items]);

  if (items.length === 0) return <div className="empty">No Work Items to graph.</div>;

  return (
    <ol className="graph-reading" aria-label="Work Items in dependency order">
      {ordered.map((item) => {
        const crit = model.criticalNodes.has(item.id);
        const blocked = blockedByLabel(model, item.id);
        const blocks = blocksLabel(model, item.id);
        const Row = item.session ? "button" : "div";
        return (
          <li key={item.id}>
            <Row
              className={`gr-node is-${item.status}${crit ? " crit" : ""}`}
              id={domId("reading", item.id)}
              {...(item.session
                ? { type: "button" as const, onClick: () => onOpenSession(item.session!.id) }
                : {})}
            >
              <div className="gr-top">
                <span className="glyph" aria-hidden="true" style={{ color: statusColor(item.status) }}>
                  {STATUS_GLYPH[item.status]}
                </span>
                <span className="id">{item.id}</span>
                <span className="sr-only">status: {statusLabel(item.status)}</span>
                {crit && <span className="crit-tag">critical path</span>}
                {item.session && (
                  <span className={`agent-pill ${item.session.status}`}>
                    <span aria-hidden="true">{agentPillText(item.session.status)}</span>
                    <span className="sr-only">{agentAccessibleText(item.session.status)}</span>
                  </span>
                )}
                {item.effort && <span className="eff">{item.effort}</span>}
              </div>
              <div className="gr-title">{item.title}</div>
              {(blocked || blocks) && (
                <div className="gr-rel">
                  {blocked && <span className="gr-blocked">{blocked}</span>}
                  {blocks && <span className="gr-blocks">{blocks}</span>}
                </div>
              )}
            </Row>
          </li>
        );
      })}
    </ol>
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
          <span
            className="track"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={total}
            aria-valuenow={done}
            aria-valuetext={`${done} of ${total} Work Items done`}
            aria-label="Project progress"
          >
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
        <Kanban items={items} source={data?.source ?? null} onOpenSession={onOpenSession} />
      ) : (
        <>
          {/* Wide screens: the spatial pan-scroll graph. Narrow screens: the same
              structure as an ordered dependency list (audit H4). CSS shows exactly
              one, so a screen reader is never given both. */}
          <div className="graph-visual">
            <GraphLegend />
            <GraphScroll>
              <Graph items={items} onOpenSession={onOpenSession} />
            </GraphScroll>
          </div>
          <GraphReading items={items} onOpenSession={onOpenSession} />
        </>
      )}
    </>
  );
}
