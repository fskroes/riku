import { useCallback, useState } from "react";
import type { Handoff } from "./types";
import { CORRECTION_DEFAULT, correction } from "./journal";

/** One card's correction box: what the user has typed, where they are leaving
 *  the card, and the append itself. State per card rather than per view, because
 *  a half-typed answer on one thread must not follow the user to another. */
export interface CorrectionBox {
  text: string;
  setText: (text: string) => void;
  handoff: Handoff;
  setHandoff: (handoff: Handoff) => void;
  /** Append what is typed. A no-op while one append is in flight, and while
   *  there is nothing to say. */
  send: () => void;
  /** Whether there is anything to append — the one emptiness rule, so the
   *  button and the append itself cannot disagree about it. */
  ready: boolean;
  sending: boolean;
  /** Why the last append did not land, for the card to show. */
  error: string | null;
}

/**
 * Answering a card in the user's own words (ADR 0013).
 *
 * `POST /api/recap/note` appends a `who:"user"` entry through the same path as
 * `riku journal note` — an explicit user action, with Riku as the user's pen.
 * Nothing is edited and nothing is deleted: the agent's word stays on disk and
 * the user's answer is appended after it.
 *
 * On success the recap is re-read rather than patched in place (`onNoted`).
 * Which entry has the last word, what that does to the card's pill, and where
 * the card then sorts are the server's to resolve, and a client that guessed at
 * them would be a second implementation of latest-wins waiting to disagree with
 * the first.
 */
export function useCorrection(cwd: string, onNoted: () => void): CorrectionBox {
  const [text, setText] = useState("");
  const [handoff, setHandoff] = useState<Handoff>(CORRECTION_DEFAULT);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const send = useCallback((): void => {
    const answer = correction(cwd, text, handoff);
    if (!answer || sending) return;
    setSending(true);
    setError(null);
    fetch("/api/recap/note", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(answer),
    })
      .then(async (res) => {
        if (!res.ok) {
          const body = (await res.json().catch(() => null)) as { error?: string } | null;
          setError(body?.error ?? "Could not append your answer");
          return;
        }
        // Cleared only once the append has actually landed: a box emptied on
        // click would lose the user's words to a failed write.
        setText("");
        setHandoff(CORRECTION_DEFAULT);
        onNoted();
      })
      .catch(() => setError("Could not reach the board"))
      .finally(() => setSending(false));
  }, [cwd, text, handoff, sending, onNoted]);

  return {
    text,
    setText,
    handoff,
    setHandoff,
    send,
    ready: correction(cwd, text, handoff) !== null,
    sending,
    error,
  };
}
