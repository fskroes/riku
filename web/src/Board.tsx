import type { Session } from "./types";
import type { OpenController } from "./useOpen";
import { abbrevTokens, causeLabel, domId, relativeAge, waitingFor } from "./format";
import { Cost, Diff, Machine, Tile, useFlash } from "./ui";

const byMostRecent = (a: Session, b: Session): number =>
  Date.parse(b.lastEventAt) - Date.parse(a.lastEventAt);

/** Oldest-waiting-first (ADR 0010): the session whose current need began longest
 *  ago sorts to the top, so the most-neglected need is always first. Falls back to
 *  last-event order if a card somehow lacks a since. */
const byOldestWaiting = (a: Session, b: Session): number => {
  const at = a.attention ? Date.parse(a.attention.since) : Date.parse(a.lastEventAt);
  const bt = b.attention ? Date.parse(b.attention.since) : Date.parse(b.lastEventAt);
  return at - bt;
};

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

/** An Attention card (ADR 0010): a triage surface, not a telemetry summary. It
 *  names the typed cause, how long the need has waited, and a bounded source-faithful
 *  evidence preview (or an honest "details on the source machine" when none is safe),
 *  plus the routing context — project, source tool, source machine, linked Work Item,
 *  optional branch. The single action is **Open session**; there is no dismiss. All
 *  causes share one visual priority — the words do the triage work. */
function AlertRow({
  session,
  now,
  open,
  onOpenPlan,
}: {
  session: Session;
  now: number;
  open: OpenController;
  onOpenPlan: OpenPlan;
}) {
  const attention = session.attention;
  const pending = open.pendingId === session.id;
  const failed = open.error?.id === session.id ? open.error.message : null;
  return (
    <div className="alert" id={domId("board", session.id)}>
      <div className="body">
        <div className="r1">
          <span className="name">{session.project}</span>
          {session.branch && <span className="rbranch">⑂ {session.branch}</span>}
          <PlanLink session={session} onOpenPlan={onOpenPlan} />
          {attention && (
            <span className="waiting" title={`Waiting since ${attention.since}`}>
              {waitingFor(attention.since, now)}
            </span>
          )}
        </div>
        <div className="cause">{attention ? causeLabel(attention.cause) : "Needs attention"}</div>
        {attention?.evidence ? (
          <div className="evidence">{attention.evidence}</div>
        ) : attention?.detailsOnSource ? (
          <div className="evidence onsource">
            Details available only on {session.machine ?? "the source machine"}
          </div>
        ) : null}
        <div className="routing">
          <Machine host={session.machine} />
        </div>
        {failed && <div className="openerr">{failed}</div>}
      </div>
      <button
        className="cta"
        type="button"
        title="Open this session in a local terminal"
        disabled={pending || !session.cwd}
        onClick={() => open.onOpen(session)}
      >
        {pending ? "Opening…" : "Open session →"}
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

  const attention = sessions.filter((s) => s.status === "attention").sort(byOldestWaiting);
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
      <Band dot="attention" label="Needs attention" count={attention.length} />
      {attention.map((s) => (
        <AlertRow key={s.id} session={s} now={now} open={open} onOpenPlan={onOpenPlan} />
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
