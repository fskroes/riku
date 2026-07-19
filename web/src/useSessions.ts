import { useEffect, useState } from "react";
import type { Session, SessionsResponse } from "./types";

interface SessionsState {
  sessions: Session[];
  connected: boolean;
}

/**
 * Live view of every Agent Session.
 *
 * Fetches the full snapshot from `/api/sessions`, then keeps it current from the
 * `/api/events` SSE stream. Each event carries a full Session, so we upsert by
 * `id` and ordering / duplication is harmless. On every (re)connect we re-fetch
 * the snapshot to resync after any gap.
 */
export function useSessions(): SessionsState {
  const [sessions, setSessions] = useState<Map<string, Session>>(new Map());
  const [connected, setConnected] = useState(false);

  useEffect(() => {
    let cancelled = false;

    async function loadSnapshot(): Promise<void> {
      try {
        const res = await fetch("/api/sessions");
        if (!res.ok) return;
        const body = (await res.json()) as SessionsResponse;
        if (cancelled) return;
        setSessions(new Map(body.sessions.map((s) => [s.id, s])));
      } catch {
        // Transient; the stream (or the next reconnect) will resync us.
      }
    }

    const upsert = (session: Session): void =>
      setSessions((prev) => {
        const next = new Map(prev);
        next.set(session.id, session);
        return next;
      });

    const remove = (id: string): void =>
      setSessions((prev) => {
        if (!prev.has(id)) return prev;
        const next = new Map(prev);
        next.delete(id);
        return next;
      });

    void loadSnapshot();

    const es = new EventSource("/api/events");
    es.onopen = () => {
      setConnected(true);
      // Resync after a reconnect (harmless on the first open).
      void loadSnapshot();
    };
    es.onerror = () => setConnected(false);
    es.addEventListener("session", (e) => {
      upsert(JSON.parse((e as MessageEvent).data) as Session);
    });
    es.addEventListener("removed", (e) => {
      const { id } = JSON.parse((e as MessageEvent).data) as { id: string };
      remove(id);
    });

    return () => {
      cancelled = true;
      es.close();
    };
  }, []);

  return { sessions: [...sessions.values()], connected };
}
