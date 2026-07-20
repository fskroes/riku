import { useEffect, useMemo, useState } from "react";
import type { ProjectRef, RelayStatus, Session } from "./types";
import { useSessions } from "./useSessions";
import { useRelay } from "./useRelay";
import { useOpen } from "./useOpen";
import { useNow } from "./ui";
import { Board } from "./Board";
import { WorkItems } from "./WorkItems";

type View = "board" | "work";

/** The number of distinct machines with a session on the board (C7) — the count the
 *  Relay pill shows. Local cards carry this host; remote cards their own. */
function machineCount(sessions: Session[]): number {
  const hosts = new Set<string>();
  for (const s of sessions) if (s.machine) hosts.add(s.machine);
  return hosts.size;
}

/** The topbar Relay pill (variant A). Grey `local only` with no Relay (zero-setup
 *  solo mode); green `relay ✓ · N machines` when subscribed and live; amber
 *  `relay … · N machines` while the board is reconnecting to the Relay. */
function RelayPill({ relay, machines }: { relay: RelayStatus; machines: number }) {
  const label = `${machines} machine${machines === 1 ? "" : "s"}`;
  if (!relay.configured) {
    return (
      <span className="relaypill off" title="No Relay configured — solo / local mode">
        <span className="dot finished" /> local only
      </span>
    );
  }
  return (
    <span
      className="relaypill"
      title={relay.connected ? "Subscribed to a Relay (C7)" : "Reconnecting to the Relay…"}
    >
      <span className={`dot ${relay.connected ? "live" : "attention"}`} />
      relay {relay.connected ? "✓" : "…"} · <span className="rl">{label}</span>
    </span>
  );
}

/** The distinct projects behind the current sessions, one per directory (a
 *  project needs a `cwd` to look up its Work Items). Sorted for a stable menu. */
function projectsOf(sessions: Session[]): ProjectRef[] {
  const byCwd = new Map<string, ProjectRef>();
  for (const s of sessions) {
    if (s.cwd && !byCwd.has(s.cwd)) byCwd.set(s.cwd, { name: s.project, cwd: s.cwd });
  }
  return [...byCwd.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/** Whether estimated costs are shown. Persisted so a subscription user, who pays no
 *  marginal per-token cost, can hide the (misleading-for-them) estimate for good. */
function useShowCost(): [boolean, () => void] {
  const [show, setShow] = useState(() => localStorage.getItem("hideCost") !== "1");
  const toggle = (): void => {
    setShow((prev) => {
      const next = !prev;
      localStorage.setItem("hideCost", next ? "0" : "1");
      return next;
    });
  };
  return [show, toggle];
}

export default function App() {
  const { sessions, connected } = useSessions();
  const relay = useRelay();
  const open = useOpen();
  const now = useNow(15000);
  const [showCost, toggleCost] = useShowCost();

  const machines = useMemo(() => machineCount(sessions), [sessions]);

  // PROTOTYPE — "?variant=" lands straight on the Work Items view (where the
  // throwaway work-graph variants mount). Remove with WorkGraphPrototype.
  const protoView: View = new URLSearchParams(window.location.search).has("variant") ? "work" : "board";
  const [view, setView] = useState<View>(protoView);
  // Cross-link focus: a session id to reveal on the Board, and a project +
  // session id to reveal in Work Items. Set when jumping between the two views.
  const [boardFocus, setBoardFocus] = useState<string | null>(null);
  const [workFocus, setWorkFocus] = useState<string | null>(null);
  const [project, setProject] = useState<ProjectRef | null>(null);

  const projects = useMemo(() => projectsOf(sessions), [sessions]);

  // Keep a valid project selected as the session set changes: default to the
  // first, and drop a selection whose project has gone away.
  useEffect(() => {
    if (projects.length === 0) {
      setProject(null);
    } else if (!project || !projects.some((p) => p.cwd === project.cwd)) {
      setProject(projects[0]);
    }
  }, [projects, project]);

  const live = sessions.filter((s) => s.status !== "finished").length;

  // Board → Work Items: open the plan for a session's project, highlight its item.
  const openPlan = (session: Session): void => {
    if (!session.cwd) return;
    setProject({ name: session.project, cwd: session.cwd });
    setWorkFocus(session.id);
    setBoardFocus(null);
    setView("work");
  };

  // Work Items → Board: reveal a linked session's card.
  const openSession = (sessionId: string): void => {
    setBoardFocus(sessionId);
    setWorkFocus(null);
    setView("board");
  };

  const go = (next: View): void => {
    setView(next);
    setBoardFocus(null);
    setWorkFocus(null);
  };

  return (
    <div className="window">
      <div className="topbar">
        <span className="logo">▦</span>
        <span className="brand">AGENT BOARD</span>
        <span className="live">{live} LIVE</span>
        <span className="seg" role="tablist" style={{ marginLeft: 8 }}>
          <button type="button" role="tab" aria-pressed={view === "board"} onClick={() => go("board")}>
            Board
          </button>
          <button type="button" role="tab" aria-pressed={view === "work"} onClick={() => go("work")}>
            Work Items
          </button>
        </span>
        <span className="spacer" />
        <button
          type="button"
          className="costtoggle"
          aria-pressed={showCost}
          onClick={toggleCost}
          title={
            showCost
              ? "Hide estimated costs (for subscription plans, which pay no marginal cost)"
              : "Show estimated costs"
          }
        >
          $ est. {showCost ? "on" : "off"}
        </button>
        <RelayPill relay={relay} machines={machines} />
        <span className="remote" title={connected ? "Live stream connected" : "Reconnecting…"}>
          <span className={`dot ${connected ? "live" : "finished"}`} />
          {connected ? "connected" : "offline"}
        </span>
      </div>

      <div className={`stage ${view === "work" ? "view-work" : "view-board"}`}>
        {view === "board" ? (
          <Board
            sessions={sessions}
            now={now}
            showCost={showCost}
            focusId={boardFocus}
            open={open}
            onOpenPlan={openPlan}
          />
        ) : (
          <WorkItems
            project={project}
            projects={projects}
            onSelectProject={(p) => {
              setProject(p);
              setWorkFocus(null);
            }}
            focusSessionId={workFocus}
            onOpenSession={openSession}
          />
        )}
      </div>
    </div>
  );
}
