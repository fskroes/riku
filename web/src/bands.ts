// How the Board's bands are derived from the live session stream: pure, so the cap
// and the count that discloses it are observable without rendering (ADR 0005).

import type { Session } from "./types";

/** Most-recent-first, by the session's last event. Shared with the Board's Active
 *  band, which orders the same way. */
export const byMostRecent = (a: Session, b: Session): number =>
  Date.parse(b.lastEventAt) - Date.parse(a.lastEventAt);

/** How many Finished Agent Sessions the Board's Finished band carries. The rest are
 *  counted but not listed: the Board answers "what needs me now?" (ADR 0005), and a
 *  page-long tail of Finished sessions buries exactly that. Mirrors the Recap's
 *  `OLDER_LIMIT` for the same reason. */
export const FINISHED_LIMIT = 5;

/** The Finished band: the rows to draw, and how many Finished Agent Sessions there are
 *  in all. `total` is carried so a reader can be told "5 of 12" rather than shown a
 *  list that implies it is everything — a cap nobody is told about would put back the
 *  blind spot the cap is meant to relieve. */
export interface FinishedBand {
  shown: Session[];
  total: number;
}

/** The most recent [`FINISHED_LIMIT`] Finished Agent Sessions, plus a focused one the
 *  cap would otherwise have hidden. */
export function finishedBand(sessions: Session[], focusId: string | null): FinishedBand {
  const finished = sessions.filter((s) => s.status === "finished").sort(byMostRecent);
  const shown = finished.slice(0, FINISHED_LIMIT);

  // A focused session outranks the cap. `focusId` means the reader arrived from a
  // Work Item's session chip to see one specific card; dropping it because it is
  // old would scroll-and-flash nothing. It keeps its place in recency order, which
  // is past the cap by definition — anything the cap kept is more recent.
  const focused = focusId ? finished.find((s) => s.id === focusId) : undefined;
  if (focused && !shown.includes(focused)) shown.push(focused);

  return { shown, total: finished.length };
}

/** The Finished band's count, disclosing the cap when one is in force: `5 of 12`,
 *  or a bare `12` when the band is showing everything. The band's own label already
 *  says "Finished", so this carries the numbers and nothing else. Mirrors the
 *  Recap's `olderLine`. */
export function finishedLine(shown: number, total: number): string {
  return shown < total ? `${shown} of ${total}` : `${total}`;
}
