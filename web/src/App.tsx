import { useEffect, useMemo, useState } from "react";
import type { ProjectRef, Session } from "./types";
import { useSessions } from "./useSessions";
import { useOpen } from "./useOpen";
import { useNow } from "./ui";
import { Board } from "./Board";
import { WorkItems } from "./WorkItems";

type View = "board" | "work";

/** The distinct projects behind the current sessions, one per directory (a
 *  project needs a `cwd` to look up its Work Items). Sorted for a stable menu. */
function projectsOf(sessions: Session[]): ProjectRef[] {
  const byCwd = new Map<string, ProjectRef>();
  for (const s of sessions) {
    if (s.cwd && !byCwd.has(s.cwd)) byCwd.set(s.cwd, { name: s.project, cwd: s.cwd });
  }
  return [...byCwd.values()].sort((a, b) => a.name.localeCompare(b.name));
}

export default function App() {
  const { sessions, connected } = useSessions();
  const open = useOpen();
  const now = useNow(15000);

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
        <span className="remote" title={connected ? "Live stream connected" : "Reconnecting…"}>
          <span className={`dot ${connected ? "live" : "finished"}`} />
          {connected ? "connected" : "offline"}
        </span>
      </div>

      <div className={`stage ${view === "work" ? "view-work" : "view-board"}`}>
        {view === "board" ? (
          <Board sessions={sessions} now={now} focusId={boardFocus} open={open} onOpenPlan={openPlan} />
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
