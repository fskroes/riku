// The journal's view model (ADR 0013): the pure half of the thread-first recap
// — labels, the derived timeline, the day lens, and what is left of resuming a
// thread. Kept out of the component so each decision is testable without a DOM,
// the way graph.ts is for the Work Items graph.
//
// Journal prose passes through here untouched and unparsed. It is data: the view
// renders it as text, and nothing in this module ever turns it into markup, a
// URL, or a command.

import type { CardResume, Handoff, RecapCard, Session, Status, Tool, Voice } from "./types";
import { ageFromSeconds, relativeAge } from "./format";

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** How a Handoff Status is spoken on a card. The wire words (`needs-you`) are the
 *  hook's and the CLI's spelling; a card says them in prose. Never "blocked",
 *  never a bare "review" — Attention owns neither of those here (CONTEXT.md). */
const HANDOFF_LABEL: Record<Handoff, string> = {
  "needs-you": "Needs you",
  "needs-review": "Needs review",
  "on-track": "On track",
};

export function handoffLabel(handoff: Handoff): string {
  return HANDOFF_LABEL[handoff];
}

/** Whose reading the card is showing. A `user` last word is a correction no agent
 *  has answered yet, which is worth saying beside the next step. */
export function whoLabel(who: Voice): string {
  return who === "user" ? "your call" : "agent suggests";
}

/** The card's entry-age label. An agent that dies uncleanly writes no stop entry,
 *  so a next step can be hours stale; labelling the age makes that visible rather
 *  than hidden (ADR 0013). Under a minute reads as words, not `0s`. */
export function ageLabel(ageSeconds: number): string {
  if (ageSeconds < 60) return "latest just now";
  return `latest ${ageFromSeconds(ageSeconds)} ago`;
}

/** One derived-timeline row: an Agent Session on this project as Riku's own
 *  transcript reading has it. Ground truth, standing beside the agent's prose. */
export interface TimelineRow {
  id: string;
  tool: Tool;
  status: Status;
  activity: string | null;
  branch: string | null;
  lastEventAt: string;
}

/** The project's derived timeline: the sessions the board knows for this
 *  directory, newest first. This is the "Where I am" column — not the journal's
 *  account of the work but Riku's, so a wrong summary is caught against it. */
export function timelineFor(sessions: Session[], cwd: string): TimelineRow[] {
  return sessions
    .filter((s) => s.cwd === cwd)
    .sort((a, b) => Date.parse(b.lastEventAt) - Date.parse(a.lastEventAt))
    .map((s) => ({
      id: s.id,
      tool: s.tool,
      status: s.status,
      activity: s.activity,
      branch: s.branch,
      lastEventAt: s.lastEventAt,
    }));
}

/** The one-line footing under a derived timeline: how much transcript it is
 *  built from, and how fresh. An empty timeline says so — a card whose prose
 *  outlived the board's 24h session window must not look like it has evidence. */
export function timelineMeta(rows: TimelineRow[], nowMs: number): string {
  if (rows.length === 0) return "no sessions in the last 24h";
  const noun = rows.length === 1 ? "session" : "sessions";
  return `${rows.length} ${noun} · last ${relativeAge(rows[0].lastEventAt, nowMs)} ago`;
}

/** What a card can offer for picking the thread back up.
 *
 *  `command` is Riku's, built from the session the store resolved; `instruction`
 *  is the author's sentence. `gone` is the honest third state: the entry names a
 *  thread this machine cannot get back into, so the sentence is what is left. */
export type ResumeOffer =
  | { kind: "command"; command: string; dir: string | null; instruction: string }
  | { kind: "gone"; instruction: string }
  | { kind: "instruction"; instruction: string };

export function resumeOffer(resume: CardResume): ResumeOffer {
  const { instruction, command, dir, sessionGone } = resume;
  // The marker wins over a command. The endpoint never sends both, and a card
  // that has just said a thread is unreachable must not also offer a paste for it.
  if (sessionGone) return { kind: "gone", instruction };
  if (command) return { kind: "command", command, dir, instruction };
  return { kind: "instruction", instruction };
}

/** One project's finished work on one day, in the day lens. */
export interface DayRow {
  project: string;
  cwd: string;
  handoff: Handoff;
  done: string[];
}

/** One day of the day lens: every project that reported finished work that day. */
export interface DayGroup {
  date: string;
  rows: DayRow[];
}

/** The day view — the secondary lens over the same cards (ADR 0013): the same
 *  `done` lines, re-keyed by the day they were reported instead of the thread
 *  they belong to. Newest day first; within a day, the cards' own order, which
 *  is Handoff Status first. Cards with no journal contribute nothing: the day
 *  view reports what was written, and invents no prose for a silent project. */
export function byDay(cards: RecapCard[]): DayGroup[] {
  const byDate = new Map<string, DayRow[]>();
  for (const card of cards) {
    if (!card.journal) continue;
    for (const day of card.journal.days) {
      const rows = byDate.get(day.date) ?? [];
      rows.push({
        project: card.project,
        cwd: card.cwd,
        handoff: card.journal.handoff,
        done: day.done,
      });
      byDate.set(day.date, rows);
    }
  }
  return [...byDate.entries()]
    .sort(([a], [b]) => (a < b ? 1 : a > b ? -1 : 0))
    .map(([date, rows]) => ({ date, rows }));
}

/** A day heading. The dates are local — the backend groups by the reader's own
 *  day so evening work does not land on tomorrow's board — so they are read back
 *  as local dates too, never through `Date.parse` of a bare `YYYY-MM-DD` (which
 *  is UTC and would shift the label a day west of Greenwich). */
export function dayLabel(date: string, nowMs: number): string {
  const [year, month, day] = date.split("-").map(Number);
  const then = new Date(year, month - 1, day);
  const today = new Date(nowMs);
  const midnight = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  const days = Math.round((midnight.getTime() - then.getTime()) / 86400000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  // Spelled out rather than left to `toLocaleDateString`, whose field order and
  // punctuation vary by locale — the rest of the board's chrome is written in
  // one language, and a day heading that reshuffles itself per machine is not
  // something a test can hold still either.
  return `${WEEKDAYS[then.getDay()]} ${then.getDate()} ${MONTHS[then.getMonth()]}`;
}

/** The headline over the cards: how many threads there are and how many want a
 *  human — which is the whole reason they are sorted the way they are.
 *
 *  With the journal off, no file was read, so there is no Handoff Status on any
 *  card and the headline says why. Reporting "nothing is waiting on you" from
 *  unread files would be asserting calm Riku has not checked for. */
export function summaryLine(cards: RecapCard[], enabled: boolean): string {
  if (cards.length === 0) return enabled ? "No threads yet." : "The journal is off.";
  const threads = `${cards.length} thread${cards.length === 1 ? "" : "s"}`;
  if (!enabled) return `${threads} · the journal is off.`;
  const wanting = cards.filter((c) => c.journal && c.journal.handoff !== "on-track").length;
  if (wanting === 0) return `${threads} · nothing is waiting on you.`;
  return wanting === 1
    ? `${threads} · 1 wants you — it is on top.`
    : `${threads} · ${wanting} want you — they are on top.`;
}

/** The heading over the older-journals list. It says the true total whenever the
 *  list is capped: a cap nobody is told about would put back the blind spot the
 *  list exists to close. */
export function olderLine(shown: number, total: number): string {
  const noun = total === 1 ? "journal" : "journals";
  const count = shown < total ? `${shown} of ${total}` : `${total}`;
  return `${count} ${noun} with no recent session`;
}
