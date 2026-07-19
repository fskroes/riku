import { useEffect, useState } from "react";
import type { Session, Tool } from "./types";
import { useSessions } from "./useSessions";
import { abbrevTokens, relativeAge, shortModel } from "./format";

/** Per-tool tile glyph + accent. One Session Source per tool (issue #5). */
const TOOL: Record<Tool, { label: string; color: string }> = {
  claude: { label: "C", color: "#E8590C" },
  codex: { label: "◆", color: "#10A37F" },
};

/** A clock that ticks every `ms` so relative ages stay fresh. */
function useNow(ms: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(id);
  }, [ms]);
  return now;
}

const byMostRecent = (a: Session, b: Session): number =>
  Date.parse(b.lastEventAt) - Date.parse(a.lastEventAt);

function Tile({ tool, small }: { tool: Tool; small?: boolean }) {
  const { label, color } = TOOL[tool];
  return (
    <span
      className={small ? "tile sm" : "tile"}
      style={{ ["--tile" as string]: color }}
    >
      {label}
    </span>
  );
}

function Meta({ session }: { session: Session }) {
  const model = shortModel(session.model);
  return (
    <span className="meta">
      {model && <span className="k">{model}</span>}
      {session.branch && <span>⑂ {session.branch}</span>}
      <span>
        ↑ {abbrevTokens(session.tokensIn)} / {abbrevTokens(session.tokensOut)}
      </span>
    </span>
  );
}

/** An Attention session: a loud full-width alert pinned to the top. */
function AlertRow({ session }: { session: Session }) {
  return (
    <div className="alert">
      <Tile tool={session.tool} />
      <div className="body">
        <div className="r1">
          <span className="name">{session.project}</span>
        </div>
        <div className="reason pillstat">💬 {session.activity ?? "Waiting for you"}</div>
        <div className="meta" style={{ marginTop: 8 }}>
          <Meta session={session} />
        </div>
      </div>
      <button className="cta" type="button" title="Deep-link lands in C6">
        Review →
      </button>
    </div>
  );
}

/** A Running / Finished session: a compact one-line row. */
function CompactRow({ session, now, done }: { session: Session; now: number; done?: boolean }) {
  return (
    <div className={done ? "row done" : "row"}>
      <Tile tool={session.tool} small />
      <div>
        <div className="name">{session.project}</div>
        <div className="branch">⑂ {session.branch ?? "—"}</div>
      </div>
      <div className="act">{done ? "✓ " : "▸ "}{session.activity ?? ""}</div>
      <div className="mini">
        <span>
          ↑{abbrevTokens(session.tokensIn)}/{abbrevTokens(session.tokensOut)}
        </span>
        <span className="age">{relativeAge(session.lastEventAt, now)}</span>
      </div>
    </div>
  );
}

function Band({ dot, label, count }: { dot: string; label: string; count: number }) {
  return (
    <div className="band">
      <span className={`dot ${dot}`} />
      <b className="label-micro">{label}</b>
      <span className="n">{count}</span>
    </div>
  );
}

export default function App() {
  const { sessions, connected } = useSessions();
  const now = useNow(15000);

  const attention = sessions.filter((s) => s.status === "attention").sort(byMostRecent);
  const active = sessions.filter((s) => s.status === "active").sort(byMostRecent);
  const finished = sessions.filter((s) => s.status === "finished").sort(byMostRecent);
  const live = attention.length + active.length;

  return (
    <div className="window">
      <div className="topbar">
        <span className="logo">▦</span>
        <span className="brand">AGENT BOARD</span>
        <span className="live">{live} LIVE</span>
        <span className="spacer" />
        <span className="remote" title={connected ? "Live stream connected" : "Reconnecting…"}>
          <span className={`dot ${connected ? "live" : "finished"}`} />
          {connected ? "connected" : "offline"}
        </span>
      </div>

      <div className="stage view-board">
        <div className="stream">
          {sessions.length === 0 ? (
            <div className="empty">No agent sessions in the last 24h.</div>
          ) : (
            <>
              <Band dot="attention" label="Needs you" count={attention.length} />
              {attention.map((s) => (
                <AlertRow key={s.id} session={s} />
              ))}

              <Band dot="active" label="Running" count={active.length} />
              {active.map((s) => (
                <CompactRow key={s.id} session={s} now={now} />
              ))}

              <Band dot="finished" label="Finished" count={finished.length} />
              {finished.map((s) => (
                <CompactRow key={s.id} session={s} now={now} done />
              ))}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
