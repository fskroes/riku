import { useCallback, useEffect, useState } from "react";
import type { WorkResponse } from "./types";

interface WorkState {
  data: WorkResponse | null;
  loading: boolean;
  error: boolean;
}

export interface UseWork extends WorkState {
  // Re-read the source now (the Retry / "couldn't refresh" action, M1). Cheap: it
  // just re-runs the fetch effect.
  refetch: () => void;
}

/**
 * The Work Items for one project directory (`cwd`), from `GET /api/work`.
 *
 * Refetches whenever the selected project changes, on `refetch()`, and on a slow
 * interval so the Work Link chips track live session status (a session going into
 * Attention, a branch switching item). `cwd === null` (no project selected) holds
 * an empty, non-loading state.
 *
 * On a failed refetch it keeps any data already on screen and flags `error` — the
 * view surfaces a quiet "couldn't refresh" cue rather than blanking the plan (M1).
 */
export function useWork(cwd: string | null): UseWork {
  const [state, setState] = useState<WorkState>({ data: null, loading: cwd !== null, error: false });
  const [tick, setTick] = useState(0);
  const refetch = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    if (cwd === null) {
      setState({ data: null, loading: false, error: false });
      return;
    }

    let cancelled = false;
    // Only show the full loading state when there is nothing on screen yet; a
    // refresh over existing data stays silent until it fails.
    setState((prev) => ({ ...prev, loading: prev.data === null, error: false }));

    async function load(): Promise<void> {
      try {
        const res = await fetch(`/api/work?cwd=${encodeURIComponent(cwd!)}`);
        if (!res.ok) throw new Error(String(res.status));
        const body = (await res.json()) as WorkResponse;
        if (!cancelled) setState({ data: body, loading: false, error: false });
      } catch {
        // Keep any previous data on screen; just flag the error.
        if (!cancelled) setState((prev) => ({ data: prev.data, loading: false, error: true }));
      }
    }

    void load();
    const id = setInterval(() => void load(), 15000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [cwd, tick]);

  return { ...state, refetch };
}
