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

/** The rail's Relay stat (C7). Solo mode (no Relay) shows a hollow grey ring;
 *  subscribed-and-live shows a solid green dot + machine count; reconnecting
 *  shows a hollow amber ring. Shape — not colour alone — separates the states
 *  (audit H3), and each carries an `aria-label` so the words are never tooltip-
 *  only. The count sits beside the dot; the rest lives in the accessible name. */
function RelayStat({ relay, machines }: { relay: RelayStatus; machines: number }) {
  if (!relay.configured) {
    const label = "Relay: none configured — solo / local mode";
    return (
      <span className="stat" role="img" aria-label={label} title={label}>
        <span className="dot ring finished" aria-hidden="true" /> ·
      </span>
    );
  }
  const machineText = `${machines} machine${machines === 1 ? "" : "s"}`;
  const label = relay.connected
    ? `Relay: subscribed and live · ${machineText}`
    : "Relay: reconnecting…";
  return (
    <span className="stat" role="img" aria-label={label} title={label}>
      <span className={`dot ${relay.connected ? "live" : "ring attention"}`} aria-hidden="true" />
      {machines}
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
  const { sessions, connected, loaded, everConnected } = useSessions();
  const relay = useRelay();
  const open = useOpen();
  const now = useNow(15000);
  const [showCost, toggleCost] = useShowCost();

  const machines = useMemo(() => machineCount(sessions), [sessions]);

  const [view, setView] = useState<View>("board");
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

  const liveLabel = `${live} live session${live === 1 ? "" : "s"}`;
  const streamLabel = connected ? "Live stream: connected" : "Live stream: reconnecting…";
  // The stream has dropped only after it once connected — the honest signal for a
  // reconnecting banner (before the first connect, the Board shows "connecting…").
  const reconnecting = everConnected && !connected;

  return (
    <div className="app">
      <aside className="rail">
        <span className="brand" role="img" aria-label="Agent Board" title="Agent Board">
          ▦
        </span>
        <nav aria-label="Views">
          <button
            type="button"
            aria-current={view === "board" ? "page" : undefined}
            onClick={() => go("board")}
          >
            Board
          </button>
          <button
            type="button"
            aria-current={view === "work" ? "page" : undefined}
            onClick={() => go("work")}
          >
            Work Items
          </button>
        </nav>
        <div className="foot">
          <span className="stat" role="img" aria-label={liveLabel} title={liveLabel}>
            <span className="dot live" aria-hidden="true" /> {live}
          </span>
          <RelayStat relay={relay} machines={machines} />
          <span className="stat" role="img" aria-label={streamLabel} title={streamLabel}>
            <span className={`dot ${connected ? "live" : "ring finished"}`} aria-hidden="true" />
          </span>
          <button
            type="button"
            className="costtoggle"
            aria-pressed={showCost}
            aria-label={showCost ? "Hide estimated costs" : "Show estimated costs"}
            onClick={toggleCost}
            title={
              showCost
                ? "Hide estimated costs (for subscription plans, which pay no marginal cost)"
                : "Show estimated costs"
            }
          >
            $
          </button>
        </div>
      </aside>

      <main className={`stage ${view === "work" ? "view-work" : "view-board"}`}>
        {reconnecting && (
          <div className="reconnecting" role="status">
            <span className="dot" aria-hidden="true" />
            Reconnecting to the live stream…
          </div>
        )}
        {view === "board" ? (
          <Board
            sessions={sessions}
            now={now}
            loading={!loaded}
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
      </main>
    </div>
  );
}
