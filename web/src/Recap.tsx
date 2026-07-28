import { useEffect, useRef, useState } from "react";
import type { CardJournal, Handoff, OlderJournal, RecapCard, Session, Status } from "./types";
import { useRecap } from "./useRecap";
import { relativeAge } from "./format";
import { Branch, Tile } from "./ui";
import {
  ageLabel,
  byDay,
  dayLabel,
  handoffLabel,
  olderLine,
  resumeOffer,
  summaryLine,
  timelineFor,
  timelineMeta,
  whoLabel,
  type TimelineRow,
} from "./journal";

/** Which lens the same recap is read through. Threads is the home — the unit is
 *  the effort, not the date; the day view survives beside it (ADR 0013). */
type Lens = "threads" | "day";

/** The Handoff Status pill. Prose, not the wire word, and the status also drives
 *  the card's accent through the same class — one status, one place it is named. */
function HandoffPill({ handoff }: { handoff: Handoff }) {
  return <span className={`hpill ${handoff}`}>{handoffLabel(handoff)}</span>;
}

/** A list of `done` lines, shared by the thread card and the day lens — the same
 *  prose, keyed by thread in one and by day in the other. */
function DoneList({ done }: { done: string[] }) {
  return (
    <ul className="donelist">
      {done.map((line, i) => (
        <li key={i}>
          <span className="chk" aria-hidden="true" />
          <span>{line}</span>
        </li>
      ))}
    </ul>
  );
}

/** How a timeline row's session state is spoken, since the row carries it as a
 *  colour and an opacity otherwise (audit M4). */
const ROW_STATE: Record<Status, string> = {
  attention: "needs attention: ",
  active: "running: ",
  finished: "finished: ",
};

/** One row of the derived timeline: what Riku's own transcript reading says this
 *  session was doing. Never the journal's account — that is the point of it. */
function TimelineEntry({ row, now }: { row: TimelineRow; now: number }) {
  return (
    <li className={`tl-row ${row.status}`}>
      <Tile tool={row.tool} small />
      <span className="tl-act" title={row.activity ?? undefined}>
        <span className="sr-only">{ROW_STATE[row.status]}</span>
        {row.activity ?? <span className="tl-quiet">no activity recorded</span>}
      </span>
      <Branch branch={row.branch} className="tl-branch" />
      <span className="age">{relativeAge(row.lastEventAt, now)}</span>
    </li>
  );
}

/** The note that stands in for a resume command: the entry names a thread this
 *  machine cannot get back into. One wording for the card and the older-journal
 *  line, because it means the same thing in both. */
function SessionGone({ inline }: { inline?: boolean }) {
  return (
    <span className="rgone">
      <span aria-hidden="true">⊘ </span>
      Session gone{inline ? " — " : " — no command to resume it. Start a fresh one with this:"}
    </span>
  );
}

/** "Where I am": the derived transcript timeline, which stays beside the prose so
 *  a wrong agent summary is caught against ground truth (ADR 0013). */
function Timeline({ rows, now }: { rows: TimelineRow[]; now: number }) {
  return (
    <section className="pseg">
      <h3 className="seg-h">Where I am</h3>
      {rows.length > 0 ? (
        <ul className="tl">
          {rows.map((row) => (
            <TimelineEntry key={row.id} row={row} now={now} />
          ))}
        </ul>
      ) : (
        <p className="tl-empty">No transcript on this machine to check the prose against.</p>
      )}
      <p className="segfoot">{timelineMeta(rows, now)} · derived from transcripts</p>
    </section>
  );
}

/** The resume block: Riku's command for the human to copy, or the author's
 *  sentence alone when there is no thread left to re-enter.
 *
 *  The command is text and a copy button, never a control that runs it — Riku
 *  does not launch a session on the board's say-so, locally or remotely (ADR
 *  0002). The copied string is the one Riku built from the resolved session; no
 *  part of the journal's prose is ever assembled into it. */
function Resume({ card }: { card: CardJournal }) {
  const offer = resumeOffer(card.resume);
  const [copied, setCopied] = useState<"idle" | "done" | "failed">("idle");
  // The label clears itself, so the timer is armed when the clipboard actually
  // settles (not when the click happened) and is dropped on unmount — a slow
  // write must not leave "copied" standing, nor fire into a gone component.
  const timer = useRef<number>();
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const copy = (text: string): void => {
    const settle = (state: "done" | "failed"): void => {
      setCopied(state);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied("idle"), 2000);
    };
    navigator.clipboard
      .writeText(text)
      .then(() => settle("done"))
      .catch(() => settle("failed"));
  };

  if (offer.kind === "command") {
    return (
      <div className="resume">
        <div className="rcmd">
          <span className="label-micro">Resume in a clean session</span>
          <code>{offer.command}</code>
          <button type="button" className="copybtn" onClick={() => copy(offer.command)}>
            {copied === "done" ? "copied" : copied === "failed" ? "select it" : "copy"}
          </button>
        </div>
        {offer.dir && <p className="rdir">in {offer.dir}</p>}
        <p className="rinstr">{offer.instruction}</p>
      </div>
    );
  }

  return (
    <div className="resume">
      {offer.kind === "gone" && (
        <p>
          <SessionGone />
        </p>
      )}
      <p className="rinstr">{offer.instruction}</p>
    </div>
  );
}

/** Why a card has no prose, which is two different problems with two different
 *  owners: the journal is switched off (nothing was read), or it is on and this
 *  project's agent has never written an entry (nothing was written). The payload
 *  keeps them apart with `enabled`, so the card must not collapse them. */
const NO_JOURNAL = {
  off: {
    done: "The journal is off, so nothing was read for this project.",
    next: "Turn the journal on and a stopping agent's next step lands here.",
  },
  empty: {
    done: "No journal entry for this project — nothing was written, so nothing is claimed.",
    next: "Wire this project's agent with the stop hook and its next step lands here.",
  },
};

/** One thread of effort: the journal's Done so far and next step, the derived
 *  timeline beside them, and the way back in. */
function ThreadCard({
  card,
  sessions,
  now,
  enabled,
}: {
  card: RecapCard;
  sessions: Session[];
  now: number;
  enabled: boolean;
}) {
  const rows = timelineFor(sessions, card.cwd);
  const journal = card.journal;
  const absent = enabled ? NO_JOURNAL.empty : NO_JOURNAL.off;
  return (
    <article className={`tcard ${journal ? journal.handoff : "nojournal"}`}>
      <header className="tcard-h">
        <h2 className="tnm" title={card.cwd}>
          {card.project}
        </h2>
        {journal ? <HandoffPill handoff={journal.handoff} /> : <span className="hpill none">No entry</span>}
      </header>
      {journal && (
        <p className="tcard-meta">
          {ageLabel(journal.ageSeconds)} · {whoLabel(journal.who)}
        </p>
      )}

      <div className="pcol">
        <section className="pseg">
          <h3 className="seg-h">Done so far</h3>
          {journal && journal.days.length > 0 ? (
            journal.days.map((day) => (
              <div key={day.date} className="doneday">
                <p className="dd-h">{dayLabel(day.date, now)}</p>
                <DoneList done={day.done} />
              </div>
            ))
          ) : (
            <p className="tl-empty">{journal ? "Nothing reported done yet." : absent.done}</p>
          )}
        </section>

        <Timeline rows={rows} now={now} />

        <section className={`pseg next ${journal ? journal.handoff : "nojournal"}`}>
          <h3 className="seg-h">
            To go further
            {journal && <span className="who-tag">{whoLabel(journal.who)}</span>}
          </h3>
          {journal ? (
            <p className={journal.who === "user" ? "nexttxt byyou" : "nexttxt"}>{journal.next}</p>
          ) : (
            <p className="tl-empty">{absent.next}</p>
          )}
        </section>
      </div>

      {journal && <Resume card={journal} />}
    </article>
  );
}

/** A journal whose sessions the board has forgotten: a line, not a card. There is
 *  no directory behind the slug it is filed under, so there is nowhere to
 *  deep-link and nothing to resume into — but a question asked three days ago is
 *  exactly what a recap must not lose. */
function OlderRow({ journal }: { journal: OlderJournal }) {
  return (
    <li className={`oldrow ${journal.handoff}`}>
      <div className="or-h">
        <HandoffPill handoff={journal.handoff} />
        <code className="or-slug" title={journal.slug}>
          {journal.slug}
        </code>
        <span className="age">{ageLabel(journal.ageSeconds)}</span>
      </div>
      <p className="or-next">{journal.next}</p>
      <p className="or-instr">
        {journal.resume.sessionGone && <SessionGone inline />}
        {journal.resume.instruction}
      </p>
    </li>
  );
}

/** The day lens: the same `done` lines, re-keyed by the day they were reported.
 *  Secondary to the threads, and derived from the same payload — there is no
 *  second reading of the journal behind it. */
function DayLens({ cards, now }: { cards: RecapCard[]; now: number }) {
  const days = byDay(cards);
  if (days.length === 0) {
    return <p className="empty">No journal entries yet — nothing to lay out by day.</p>;
  }
  return (
    <div className="daylens">
      {days.map((day) => (
        <section key={day.date} className="daygroup">
          <h2 className="dg-h">
            {dayLabel(day.date, now)} <span className="dg-date">{day.date}</span>
          </h2>
          {day.rows.map((row) => (
            <div key={`${day.date}-${row.cwd}`} className={`dgrow ${row.handoff}`}>
              <div className="dgr-h">
                <span className="tnm" title={row.cwd}>
                  {row.project}
                </span>
                <HandoffPill handoff={row.handoff} />
              </div>
              <DoneList done={row.done} />
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

/**
 * The recap (ADR 0013): one card per thread of effort, ordered needs-you →
 * needs-review → on-track by the server, each carrying the agent's Done so far,
 * the derived timeline beside it, the next step, how stale the entry is, and the
 * command Riku built to get back in.
 *
 * Every piece of journal prose on this surface is rendered as a text node. It is
 * untrusted input — written by an agent, into a file any local process can append
 * to — so it is data here and nothing else: never markup, never a URL, never
 * anything the board executes.
 */
export function Recap({ sessions, now }: { sessions: Session[]; now: number }) {
  const { data, loading, error, refetch } = useRecap();
  const [lens, setLens] = useState<Lens>("threads");

  if (!data && loading) {
    return (
      <div className="board-loading">
        <div className="connecting" role="status">
          <span className="dot doing" aria-hidden="true" /> Reading the journal…
        </div>
        <div className="skeleton sk-card" aria-hidden="true" />
        <div className="skeleton sk-row" aria-hidden="true" />
      </div>
    );
  }

  // A labelled, announced failure panel — distinct from the settled empty note
  // below, which says something true about the data rather than about the read
  // (audit M1). Same shape and wording as the Work Items view's.
  if (!data) {
    return (
      <div className="recap">
        <div className="state-block error" role="alert">
          <span className="ico" aria-hidden="true">
            ⚠
          </span>
          <div className="state-body">
            <b>Couldn’t read the recap.</b>
            <span className="detail">The board didn’t respond.</span>
          </div>
          <button type="button" className="state-action" onClick={refetch}>
            Retry
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="recap">
      <header className="recap-head">
        <div>
          <span className="label-micro">Recap</span>
          <h1>{summaryLine(data.cards, data.enabled)}</h1>
        </div>
        <div className="seg small" role="group" aria-label="Recap lens">
          <button
            type="button"
            aria-pressed={lens === "threads"}
            onClick={() => setLens("threads")}
            title="One card per thread of effort"
          >
            Threads
          </button>
          <button
            type="button"
            aria-pressed={lens === "day"}
            onClick={() => setLens("day")}
            title="The same entries, laid out by the day they were reported"
          >
            Day
          </button>
        </div>
      </header>

      {error && (
        <div className="stale-refresh" role="status">
          <span className="dot" aria-hidden="true" />
          Couldn’t refresh — showing the last reading.
          <button type="button" onClick={refetch}>
            Retry
          </button>
        </div>
      )}

      {!data.enabled && (
        <div className="journaloff" role="note">
          <p>
            <b>The project journal is off.</b> Nothing is read from disk while it is — these cards are
            the derived timeline alone.
          </p>
          <code>riku config set journal.enabled true</code>
        </div>
      )}

      {lens === "day" ? (
        <DayLens cards={data.cards} now={now} />
      ) : data.cards.length === 0 ? (
        <p className="empty">No agent sessions in the last 24h, so no threads to recap.</p>
      ) : (
        <div className="tgrid">
          {data.cards.map((card) => (
            <ThreadCard
              key={card.cwd}
              card={card}
              sessions={sessions}
              now={now}
              enabled={data.enabled}
            />
          ))}
        </div>
      )}

      {data.older.length > 0 && (
        <section className="older">
          <div className="band">
            <span className="dot finished" aria-hidden="true" />
            <b className="label-micro">Older journals</b>
            <span className="n">{olderLine(data.older.length, data.olderTotal)}</span>
          </div>
          <ul className="oldlist">
            {data.older.map((journal) => (
              <OlderRow key={journal.slug} journal={journal} />
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
