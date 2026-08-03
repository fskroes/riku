// How a Session's Sub-agent roster reads on a card: which count the fold's summary
// line shows, the words it says it in, and the text of each row under it. Pure, so
// the two summary states and the absent-Errand blank are observable without
// rendering — the same discipline `bands.ts` holds for the Board's caps.

import { abbrevTokens, formatCost, shortModel } from "./format";
import type { SubAgent } from "./types";

/** The fold's summary line: which of the two states it is in, the count that state
 *  shows, and the words that carry it. */
export interface RosterBadge {
  /** The number the summary states. */
  count: number;
  /** Any Sub-agent still running — the pulsing, accented state. Otherwise the summary
   *  is still and dimmed, and its count is the roster total rather than a zero. */
  running: boolean;
  /** The summary line's own words, stating *which* count it is showing. Read off a
   *  shut fold and heard as the disclosure's accessible name, since the two states
   *  differ by colour and motion alone otherwise, and neither a screen reader nor a
   *  reduced-motion reader has those. */
  label: string;
}

/** One row under an opened fold. */
export interface RosterRow {
  /** The row's stable identity for rendering. This is the *spawn key*, not the
   *  Sub-agent's own id: a row the parent alone established carries the spawn key as
   *  its id and adopts the Sub-agent's own once its file is discovered, so keying on
   *  `id` would remount the row the moment its spend arrived. */
  id: string;
  /** The Errand, verbatim, or `null` for a Sub-agent whose source named no purpose.
   *  A blank, never a placeholder: a label must not look like content when the
   *  source gave none. */
  errand: string | null;
  /** `Running`, or `Finished` carrying the source's own outcome word when it stated
   *  one: `Finished · failed`. Never inferred from prose. */
  state: string;
  /** Whether this row is still running, for the row's own dot. */
  running: boolean;
  /** This Sub-agent's own spend: `↑1.2k ↓340`. */
  tokens: string;
  /** Its own cost, priced at its own model, or `null` when that model is unpriced. */
  cost: string | null;
  /** The model it ran, shortened, or `null` when the source named none. */
  model: string | null;
}

const noun = (n: number): string => (n === 1 ? "sub-agent" : "sub-agents");

/** The fold's summary line, or `null` for a session that never fanned out — a fold
 *  that renders for everything says nothing when it is there. */
export function rosterBadge(roster: SubAgent[]): RosterBadge | null {
  if (roster.length === 0) return null;
  const running = roster.filter((a) => a.state === "running").length;
  if (running > 0) {
    return { count: running, running: true, label: `${running} ${noun(running)} running` };
  }
  const total = roster.length;
  return {
    count: total,
    running: false,
    label: `${total} ${noun(total)} in all, none running`,
  };
}

/** The fold's rows, in the order the Sub-agents were sent out — reading it top to
 *  bottom follows what the agent actually did, so the source's order is preserved
 *  rather than re-sorted by state or recency. */
export function rosterRows(roster: SubAgent[]): RosterRow[] {
  return roster.map((a) => ({
    id: a.spawnKey,
    errand: a.errand,
    state: rowState(a),
    running: a.state === "running",
    tokens: `↑${abbrevTokens(a.tokensIn)} ↓${abbrevTokens(a.tokensOut)}`,
    cost: formatCost(a.costUsd),
    model: shortModel(a.model),
  }));
}

/** `Running`, or `Finished` with the source's own word appended when it said one.
 *  A running Sub-agent's outcome is ignored even if one is present: what it says now
 *  is that it is running (a Sub-agent can resume after finishing, so the roster
 *  carries the latest word, not a final one). */
function rowState(a: SubAgent): string {
  if (a.state === "running") return "Running";
  return a.outcome ? `Finished · ${a.outcome}` : "Finished";
}
