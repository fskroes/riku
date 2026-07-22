// Small display helpers shared by the Board.

import type { AttentionCause } from "./types";

/** The label for each Attention cause. Text carries the triage meaning; all causes
 *  share one visual priority (ADR 0010), so there is no per-cause colour or icon. */
const CAUSE_LABEL: Record<AttentionCause, string> = {
  approval: "Approval required",
  answer: "Answer required",
  review: "Review required",
  error: "Session error",
  input: "Input required",
};

/** The human label for an Attention cause. */
export function causeLabel(cause: AttentionCause): string {
  return CAUSE_LABEL[cause];
}

/** How long the current need has waited, phrased for a card: `waiting 3m`. */
export function waitingFor(sinceIso: string, nowMs: number): string {
  return `waiting ${relativeAge(sinceIso, nowMs)}`;
}

/** Relative age like `5s`, `3m`, `2h`, `1d` from an ISO timestamp. */
export function relativeAge(iso: string, nowMs: number): string {
  const seconds = Math.max(0, Math.floor((nowMs - Date.parse(iso)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/** Token count with a `k` abbreviation: `760`, `4.7k`, `27.9k`, `128k`. */
export function abbrevTokens(n: number): string {
  if (n < 1000) return `${n}`;
  const k = n / 1000;
  const str = k < 100 ? k.toFixed(1) : k.toFixed(0);
  return `${str.replace(/\.0$/, "")}k`;
}

/** The source badge label for a project's Work Items. */
export function sourceLabel(source: "workMd" | "github"): string {
  return source === "workMd" ? "WORK.md" : "GitHub Issues";
}

/** A DOM-safe element id from an arbitrary Work Item id (`#5` → `item-5`). */
export function domId(prefix: string, raw: string): string {
  return `${prefix}-${raw.replace(/[^A-Za-z0-9_-]/g, "-")}`;
}

/** An estimated cost as a compact USD string: `<$0.01`, `$0.42`, `$12.30`, `$340`.
 *  `null` when the session has no priced cost. Always paired with an "est." label. */
export function formatCost(usd: number | null): string | null {
  if (usd == null) return null;
  if (usd < 0.01) return "<$0.01";
  if (usd < 100) return `$${usd.toFixed(2)}`;
  return `$${Math.round(usd)}`;
}

/** Turn a model id like `claude-opus-4-8` into `Opus 4.8`; falls back to raw. */
export function shortModel(model: string | null): string | null {
  if (!model) return model;
  const m = model.match(/(opus|sonnet|haiku|fable)-(\d+)(?:-(\d+))?/i);
  if (!m) return model;
  const family = m[1][0].toUpperCase() + m[1].slice(1).toLowerCase();
  const version = m[3] ? `${m[2]}.${m[3]}` : m[2];
  return `${family} ${version}`;
}
