import type { Session } from "./types";
import { abbrevTokens, domId, relativeAge } from "./format";
import { Meta, Tile, useFlash } from "./ui";

/** The glyph + headline for each Attention cause (issue #7). Falls back to
 *  waiting when a card is in Attention without an explicit reason. */
const REASON: Record<NonNullable<Session["attentionReason"]>, { icon: string; label: string }> = {
  waiting: { icon: "💬", label: "Waiting for you" },
  error: { icon: "⚠", label: "Exited with error" },
};

const byMostRecent = (a: Session, b: Session): number =>
  Date.parse(b.lastEventAt) - Date.parse(a.lastEventAt);

/** A "view in plan" button, shown when the session's project has Work Items to
 *  cross-link to. Jumps to the Work Items view and highlights the linked item. */
function PlanLink({ session, onOpenPlan }: { session: Session; onOpenPlan: OpenPlan }) {
  if (!session.cwd) return null;
  return (
    <button
      className="planlink"
      type="button"
      title="View this session in the project's Work Items"
      onClick={() => onOpenPlan(session)}
    >
      ▤ plan ↗
    </button>
  );
}

/** An Attention session: a loud full-width alert pinned to the top. */
function AlertRow({ session, onOpenPlan }: { session: Session; onOpenPlan: OpenPlan }) {
  const { icon, label } = REASON[session.attentionReason ?? "waiting"];
  return (
    <div className="alert" id={domId("board", session.id)}>
      <Tile tool={session.tool} />
      <div className="body">
        <div className="r1">
          <span className="name">{session.project}</span>
          <PlanLink session={session} onOpenPlan={onOpenPlan} />
        </div>
        <div className="reason pillstat">
          {icon} {label}
          {session.activity && <span className="detail"> · {session.activity}</span>}
        </div>
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
function CompactRow({
  session,
  now,
  done,
  onOpenPlan,
}: {
  session: Session;
  now: number;
  done?: boolean;
  onOpenPlan: OpenPlan;
}) {
  return (
    <div className={done ? "row done" : "row"} id={domId("board", session.id)}>
      <Tile tool={session.tool} small />
      <div>
        <div className="name">{session.project}</div>
        <div className="branch">⑂ {session.branch ?? "—"}</div>
      </div>
      <div className="act">
        {done ? "✓ " : "▸ "}
        {session.activity ?? ""}
      </div>
      <div className="mini">
        <PlanLink session={session} onOpenPlan={onOpenPlan} />
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

export type OpenPlan = (session: Session) => void;

/** The attention-first stream (ADR 0005): alerts pinned up top, running sessions
 *  compact, finished dimmed below. `focusId` scrolls to and flashes one card when
 *  arriving from a Work Item's session chip. */
export function Board({
  sessions,
  now,
  focusId,
  onOpenPlan,
}: {
  sessions: Session[];
  now: number;
  focusId: string | null;
  onOpenPlan: OpenPlan;
}) {
  useFlash(focusId ? domId("board", focusId) : null);

  const attention = sessions.filter((s) => s.status === "attention").sort(byMostRecent);
  const active = sessions.filter((s) => s.status === "active").sort(byMostRecent);
  const finished = sessions.filter((s) => s.status === "finished").sort(byMostRecent);

  if (sessions.length === 0) {
    return (
      <div className="stream">
        <div className="empty">No agent sessions in the last 24h.</div>
      </div>
    );
  }

  return (
    <div className="stream">
      <Band dot="attention" label="Needs you" count={attention.length} />
      {attention.map((s) => (
        <AlertRow key={s.id} session={s} onOpenPlan={onOpenPlan} />
      ))}

      <Band dot="active" label="Running" count={active.length} />
      {active.map((s) => (
        <CompactRow key={s.id} session={s} now={now} onOpenPlan={onOpenPlan} />
      ))}

      <Band dot="finished" label="Finished" count={finished.length} />
      {finished.map((s) => (
        <CompactRow key={s.id} session={s} now={now} done onOpenPlan={onOpenPlan} />
      ))}
    </div>
  );
}
