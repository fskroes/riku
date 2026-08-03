import type { Session } from "./types";
import type { OpenController } from "./useOpen";
import { byMostRecent, finishedBand, finishedLine } from "./bands";
import { causeLabel, domId, relativeAge, waitingFor } from "./format";
import { Branch, Cost, Diff, Machine, SubAgentFold, Tile, Tokens, useFlash } from "./ui";

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
      aria-label="View this session in the project's Work Items"
      title="View this session in the project's Work Items"
      onClick={() => onOpenPlan(session)}
    >
      <span aria-hidden="true">▤</span> plan <span aria-hidden="true">↗</span>
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
      aria-label="Open this session in a local terminal"
      title="Open this session in a local terminal"
      disabled={pending}
      onClick={() => open.onOpen(session)}
    >
      {pending ? "opening…" : (
        <>
          open <span aria-hidden="true">↗</span>
        </>
      )}
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
          <span className="name" title={session.project}>
            {session.project}
          </span>
          <Branch branch={session.branch} className="rbranch" />
          <PlanLink session={session} onOpenPlan={onOpenPlan} />
          {attention && (
            <span className="waiting" title={`Waiting since ${attention.since}`}>
              {waitingFor(attention.since, now)}
            </span>
          )}
        </div>
        <div className="cause">{attention ? causeLabel(attention.cause) : "Needs attention"}</div>
        {attention?.evidence ? (
          <div className="evidence" title={attention.evidence}>
            {attention.evidence}
          </div>
        ) : attention?.detailsOnSource ? (
          <div className="evidence onsource">
            Details available only on {session.machine ?? "the source machine"}
          </div>
        ) : null}
        <div className="routing">
          <Machine host={session.machine} />
        </div>
        {/* A session can need attention *and* still be fanning work out (a
            non-Task wait alongside live Sub-agents), so the fold is drawn here too:
            it follows the card's own lines rather than sitting among the routing
            context, which is what makes it read as subordinate to them. */}
        <SubAgentFold roster={session.subAgentRoster} />
        {failed && <div className="openerr">{failed}</div>}
      </div>
      <button
        className="cta"
        type="button"
        title="Open this session in a local terminal"
        disabled={pending || !session.cwd}
        onClick={() => open.onOpen(session)}
      >
        {pending ? "Opening…" : (
          <>
            Open session <span aria-hidden="true">→</span>
          </>
        )}
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
        <div className="name" title={session.project}>
          {session.project}
        </div>
        {session.branch ? (
          <Branch branch={session.branch} className="branch" />
        ) : (
          <span className="branch" aria-label="no branch">
            <span aria-hidden="true">⑂ </span>—
          </span>
        )}
      </div>
      <div className="act" title={session.activity ?? undefined}>
        <span className="sr-only">{done ? "Finished: " : "Running: "}</span>
        <span aria-hidden="true">{done ? "✓ " : "▸ "}</span>
        {session.activity ?? ""}
      </div>
      <div className="mini">
        {failed && (
          <span className="openerr" title={failed}>
            {failed}
          </span>
        )}
        <OpenLink session={session} open={open} />
        <PlanLink session={session} onOpenPlan={onOpenPlan} />
        <Diff diff={session.diff} />
        <Tokens tokensIn={session.tokensIn} tokensOut={session.tokensOut} />
        <Cost usd={session.costUsd} show={showCost} />
        <Machine host={session.machine} />
        <span className="age">{relativeAge(session.lastEventAt, now)}</span>
      </div>
      {/* A line of the row's own grid rather than a member of its stat cluster, so
          an opened fold runs the width of the row it hangs under. */}
      <SubAgentFold roster={session.subAgentRoster} />
    </div>
  );
}

function Band({ dot, label, count }: { dot: string; label: string; count: number | string }) {
  return (
    <div className="band">
      <span className={`dot ${dot}`} />
      <b className="label-micro">{label}</b>
      <span className="n">{count}</span>
    </div>
  );
}

export type OpenPlan = (session: Session) => void;

/** The attention-first focus board (ADR 0005): the oldest need is the primary
 *  decision, later needs queue beside it, running sessions stay compact, and
 *  finished sessions dim below the queue. `focusId` scrolls to and flashes one
 *  card when arriving from a Work Item's session chip. */
export function Board({
  sessions,
  now,
  loading,
  showCost,
  focusId,
  open,
  onOpenPlan,
}: {
  sessions: Session[];
  now: number;
  loading?: boolean;
  showCost: boolean;
  focusId: string | null;
  open: OpenController;
  onOpenPlan: OpenPlan;
}) {
  useFlash(focusId ? domId("board", focusId) : null);

  const attention = sessions.filter((s) => s.status === "attention").sort(byOldestWaiting);
  const active = sessions.filter((s) => s.status === "active").sort(byMostRecent);
  // Finished is capped and the cap is disclosed (issue #64): the tail of a long day
  // would otherwise push the queue off screen. `focusId` is handed in so a card
  // arrived at from a Work Item's session chip survives the cap.
  const finished = finishedBand(sessions, focusId);

  // Before the first snapshot resolves, "connecting…" — not the settled empty
  // state — so the Board never flashes "no sessions" on first paint (audit M6).
  if (sessions.length === 0 && loading) {
    return (
      <div className="board-loading">
        <div className="connecting" role="status">
          <span className="dot doing" aria-hidden="true" /> Connecting to the live stream…
        </div>
        <div className="skeleton sk-card" aria-hidden="true" />
        <div className="skeleton sk-row" aria-hidden="true" />
        <div className="skeleton sk-row" aria-hidden="true" />
      </div>
    );
  }

  if (sessions.length === 0) {
    return (
      <div className="stream">
        <div className="empty">No agent sessions in the last 24h.</div>
      </div>
    );
  }

  const [next, ...later] = attention;

  return (
    <div className="focus-board">
      <main>
        <header className="board-heading">
          <div>
            <span className="label-micro">Next decision</span>
            <h1>{next ? "One thing at a time." : "The desk is clear."}</h1>
          </div>
          <p>{later.length ? `${later.length} more waiting behind this` : "No other needs queued"}</p>
        </header>

        {next ? (
          <div className="focus-card">
            <AlertRow session={next} now={now} open={open} onOpenPlan={onOpenPlan} />
          </div>
        ) : (
          <div className="empty">No Agent Session needs attention.</div>
        )}

        <section className="focus-running">
          <Band dot="active" label="Running in the background" count={active.length} />
          {active.map((session) => (
            <CompactRow
              key={session.id}
              session={session}
              now={now}
              open={open}
              showCost={showCost}
              onOpenPlan={onOpenPlan}
            />
          ))}
        </section>
      </main>

      <aside>
        <Band dot="attention" label="Up next" count={later.length} />
        {later.map((session) => (
          <AlertRow
            key={session.id}
            session={session}
            now={now}
            open={open}
            onOpenPlan={onOpenPlan}
          />
        ))}

        <Band
          dot="finished"
          label="Finished"
          count={finishedLine(finished.shown.length, finished.total)}
        />
        {finished.shown.map((session) => (
          <CompactRow
            key={session.id}
            session={session}
            now={now}
            open={open}
            showCost={showCost}
            done
            onOpenPlan={onOpenPlan}
          />
        ))}
      </aside>
    </div>
  );
}
