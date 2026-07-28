import type { WorkResponse } from "./types";
import { usePolled, type Polled } from "./usePolled";

/** How often the plan is re-read, so the Work Link chips track live session
 *  status (a session going into Attention, a branch switching item). */
const EVERY = 15000;

export type UseWork = Polled<WorkResponse>;

/**
 * The Work Items for one project directory (`cwd`), from `GET /api/work`.
 *
 * Refetches whenever the selected project changes, on `refetch()`, and on the
 * poll interval. `cwd === null` (no project selected) holds an empty,
 * non-loading state; a failed refetch keeps the plan on screen and flags
 * `error`, which the view surfaces as a quiet "couldn't refresh" cue (M1).
 */
export function useWork(cwd: string | null): UseWork {
  const url = cwd === null ? null : `/api/work?cwd=${encodeURIComponent(cwd)}`;
  return usePolled<WorkResponse>(url, EVERY);
}
