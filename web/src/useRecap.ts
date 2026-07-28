import type { RecapResponse } from "./types";
import { usePolled, type Polled } from "./usePolled";

/** How often the journal is re-read. Entries are appended when a session stops
 *  or the user answers a card — minutes apart, not seconds. */
const EVERY = 15000;

/**
 * The journal-derived recap, from `GET /api/recap` (ADR 0013).
 *
 * Polled rather than streamed: the SSE stream carries live session events and
 * has no business carrying prose. A failed refresh keeps the last reading on
 * screen — a recap that blanks on one bad poll loses the very question it exists
 * to keep visible.
 */
export function useRecap(): Polled<RecapResponse> {
  return usePolled<RecapResponse>("/api/recap", EVERY);
}
