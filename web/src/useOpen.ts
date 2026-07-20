import { useCallback, useState } from "react";
import type { Session } from "./types";

/** The last open failure: which session, and a human-facing reason to show. */
export interface OpenError {
  id: string;
  message: string;
}

/** Deep-linking a session into its local terminal (C6). `onOpen` POSTs to the
 *  board, which resumes the session on this machine; `pendingId` and `error` let a
 *  card reflect the in-flight / failed launch. */
export interface OpenController {
  onOpen: (session: Session) => void;
  pendingId: string | null;
  error: OpenError | null;
}

export function useOpen(): OpenController {
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [error, setError] = useState<OpenError | null>(null);

  const onOpen = useCallback((session: Session): void => {
    setPendingId(session.id);
    setError(null);
    fetch(`/api/sessions/${encodeURIComponent(session.id)}/open`, { method: "POST" })
      .then(async (res) => {
        if (res.ok) return;
        const body = (await res.json().catch(() => null)) as { error?: string } | null;
        setError({ id: session.id, message: body?.error ?? "Could not open the session" });
      })
      .catch(() => setError({ id: session.id, message: "Could not reach the board" }))
      // Clear the spinner only if a newer open has not already claimed it.
      .finally(() => setPendingId((cur) => (cur === session.id ? null : cur)));
  }, []);

  return { onOpen, pendingId, error };
}
