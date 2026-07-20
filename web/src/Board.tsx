import type { Session } from "./types";
import type { OpenController } from "./useOpen";
import { abbrevTokens, domId, relativeAge } from "./format";
import { Cost, Diff, Machine, Meta, Tile, useFlash } from "./ui";

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

/** Deep-link a session into its local terminal (C6). Disabled while its own launch
 *  is in flight; without a `cwd` there is no workspace to resume into. */
function OpenLink({ session, open }: { session: Session; open: OpenController }) {
  if (!session.cwd) return null;
  const pending = open.pendingId === session.id;
  return (
    <button
      className="openlink"
      type="button"
      title="Open this session in a local terminal"
      disabled={pending}
      onClick={() => open.onOpen(session)}
    >
      {pending ? "opening…" : "open ↗"}
    </button>
  );
}

/** An Attention session: a loud full-width alert pinned to the top. */
function AlertRow({
  session,
  open,
  showCost,
  onOpenPlan,
}: {
  session: Session;
  open: OpenController;
  showCost: boolean;
  onOpenPlan: OpenPlan;
}) {
  const { icon, label } = REASON[session.attentionReason ?? "waiting"];
  const pending = open.pendingId === session.id;
  const failed = open.error?.id === session.id ? open.error.message : null;
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
          <Meta session={session} showCost={showCost} />
        </div>
        {failed && <div className="openerr">{failed}</div>}
      </div>
      <button
        className="cta"
        type="button"
        title="Open this session in a local terminal to respond"
        disabled={pending || !session.cwd}
        onClick={() => open.onOpen(session)}
      >
        {pending ? "Opening…" : "Review →"}
      </button>
    </div>
  );
}

/** A Running / Finished session: a compact one-line row. */
function CompactRow({
  session,
  now,
  showCost,
  done,
  open,
  onOpenPlan,
}: {
  session: Session;
  now: number;
  showCost: boolean;
  done?: boolean;
  open: OpenController;
  onOpenPlan: OpenPlan;
}) {
  const failed = open.error?.id === session.id ? open.error.message : null;
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
        {failed && <span className="openerr" title={failed}>open failed</span>}
        <OpenLink session={session} open={open} />
        <PlanLink session={session} onOpenPlan={onOpenPlan} />
        <Diff diff={session.diff} />
        <span>
          ↑{abbrevTokens(session.tokensIn)}/{abbrevTokens(session.tokensOut)}
        </span>
        <Cost usd={session.costUsd} show={showCost} />
        <Machine host={session.machine} />
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
  showCost,
  focusId,
  open,
  onOpenPlan,
}: {
  sessions: Session[];
  now: number;
  showCost: boolean;
  focusId: string | null;
  open: OpenController;
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
        <AlertRow key={s.id} session={s} open={open} showCost={showCost} onOpenPlan={onOpenPlan} />
      ))}

      <Band dot="active" label="Running" count={active.length} />
      {active.map((s) => (
        <CompactRow key={s.id} session={s} now={now} open={open} showCost={showCost} onOpenPlan={onOpenPlan} />
      ))}

      <Band dot="finished" label="Finished" count={finished.length} />
      {finished.map((s) => (
        <CompactRow key={s.id} session={s} now={now} open={open} showCost={showCost} done onOpenPlan={onOpenPlan} />
      ))}
    </div>
  );
}
