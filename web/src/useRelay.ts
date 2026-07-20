import { useEffect, useState } from "react";
import type { RelayStatus } from "./types";

const LOCAL_ONLY: RelayStatus = { configured: false, connected: false };

/**
 * The board's Relay-subscription status, for the topbar pill (C7).
 *
 * Polled from `/api/relay` on a short interval so the pill flips between
 * connected / reconnecting as the board's own subscription to the Relay comes and
 * goes — a slower cadence than the session stream, since it only drives one chip.
 * A board with no Relay configured stays `local only` (zero-setup solo mode).
 */
export function useRelay(): RelayStatus {
  const [status, setStatus] = useState<RelayStatus>(LOCAL_ONLY);

  useEffect(() => {
    let cancelled = false;

    async function load(): Promise<void> {
      try {
        const res = await fetch("/api/relay");
        if (!res.ok) return;
        const body = (await res.json()) as RelayStatus;
        if (!cancelled) setStatus(body);
      } catch {
        // Transient; the next poll retries.
      }
    }

    void load();
    const id = setInterval(load, 5000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  return status;
}
