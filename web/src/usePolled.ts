import { useCallback, useEffect, useState } from "react";

interface PolledState<T> {
  data: T | null;
  loading: boolean;
  error: boolean;
}

export interface Polled<T> extends PolledState<T> {
  /** Re-read the source now — the "couldn't refresh" retry. Cheap: it just
   *  re-runs the fetch effect. */
  refetch: () => void;
}

/**
 * A JSON endpoint the board reads on a slow interval: the shape shared by the
 * Work Items plan and the journal recap.
 *
 * Both are periodic reads of data that changes on a human timescale — a plan
 * edited in an editor, a journal appended when a session stops — so neither
 * belongs on the SSE stream, which carries live session events.
 *
 * A failed refresh keeps whatever is already on screen and flags `error`, so the
 * view can show a quiet "couldn't refresh" cue rather than blanking (audit M1):
 * stale data with a warning beats an empty panel. `url === null` means there is
 * nothing to read yet (no project selected) and holds an empty, settled state.
 */
export function usePolled<T>(url: string | null, intervalMs: number): Polled<T> {
  const [state, setState] = useState<PolledState<T>>({
    data: null,
    loading: url !== null,
    error: false,
  });
  const [tick, setTick] = useState(0);
  const refetch = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    if (url === null) {
      setState({ data: null, loading: false, error: false });
      return;
    }

    let cancelled = false;
    // Only show the full loading state when there is nothing on screen yet; a
    // refresh over existing data stays silent until it fails.
    setState((prev) => ({ ...prev, loading: prev.data === null, error: false }));

    async function load(): Promise<void> {
      try {
        const res = await fetch(url!);
        if (!res.ok) throw new Error(String(res.status));
        const body = (await res.json()) as T;
        if (!cancelled) setState({ data: body, loading: false, error: false });
      } catch {
        // Keep any previous data on screen; just flag the error.
        if (!cancelled) setState((prev) => ({ data: prev.data, loading: false, error: true }));
      }
    }

    void load();
    const id = setInterval(() => void load(), intervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [url, intervalMs, tick]);

  return { ...state, refetch };
}
