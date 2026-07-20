import { useEffect, useState } from "react";
import type { DiffStat, Session, Tool } from "./types";
import { abbrevTokens, formatCost, shortModel } from "./format";

/** Per-tool tile glyph + accent. One Session Source per tool (issue #5). */
export const TOOL: Record<Tool, { label: string; color: string }> = {
  claude: { label: "C", color: "#E8590C" },
  codex: { label: "◆", color: "#10A37F" },
};

/** A clock that ticks every `ms` so relative ages stay fresh. */
export function useNow(ms: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(id);
  }, [ms]);
  return now;
}

/** The tool glyph tile shown on every session card and Work Link chip. */
export function Tile({ tool, small }: { tool: Tool; small?: boolean }) {
  const { label, color } = TOOL[tool];
  return (
    <span className={small ? "tile sm" : "tile"} style={{ ["--tile" as string]: color }}>
      {label}
    </span>
  );
}

/** The machine chip (C7): which machine an Agent Session is on. Shown on every
 *  card so a mixed local + Relay board labels sessions consistently. Renders
 *  nothing for an unstamped (pre-C7) session. */
export function Machine({ host }: { host: string | null }) {
  if (!host) return null;
  return (
    <span className="machine" title={`Running on ${host}`}>
      <span className="mdot" /> {host}
    </span>
  );
}

/** The git `+/-` stat for a card (C5). Renders nothing when the session has no
 *  repo diff (e.g. its cwd is not a git checkout). */
export function Diff({ diff }: { diff: DiffStat | null }) {
  if (!diff) return null;
  return (
    <span className="diff" title="Lines changed on this branch (uncommitted + since default branch)">
      <span className="add">+{diff.added}</span>
      <span className="del">−{diff.removed}</span>
    </span>
  );
}

/** The estimated cost chip (C5), always labelled "est.". Renders nothing when the
 *  cost toggle is off (subscription sessions) or the model has no list price. */
export function Cost({ usd, show }: { usd: number | null; show: boolean }) {
  const text = formatCost(usd);
  if (!show || text == null) return null;
  return (
    <span className="cost" title="Estimated from public list token prices — subscription plans pay no marginal cost">
      {text} <span className="est">est.</span>
    </span>
  );
}

/** The mono model · branch · tokens · diff · cost line under a session's headline.
 *  `showCost` gates the estimate chip (off for subscription sessions). */
export function Meta({ session, showCost }: { session: Session; showCost: boolean }) {
  const model = shortModel(session.model);
  return (
    <span className="meta">
      {model && <span className="k">{model}</span>}
      {session.branch && <span>⑂ {session.branch}</span>}
      <span>
        ↑ {abbrevTokens(session.tokensIn)} / {abbrevTokens(session.tokensOut)}
      </span>
      <Diff diff={session.diff} />
      <Cost usd={session.costUsd} show={showCost} />
      <Machine host={session.machine} />
    </span>
  );
}

/**
 * Scroll a freshly-focused element into view and flash it. Used for cross-linking
 * between the Board and Work Items: the target element carries a stable DOM id and
 * `focusId` is the one to reveal. Clears the flash after the animation.
 */
export function useFlash(focusId: string | null): void {
  useEffect(() => {
    if (!focusId) return;
    // Defer to the next frame so the target view has rendered.
    const raf = requestAnimationFrame(() => {
      const el = document.getElementById(focusId);
      if (!el) return;
      el.scrollIntoView({ block: "center", behavior: "smooth" });
      el.classList.add("flash");
      window.setTimeout(() => el.classList.remove("flash"), 2000);
    });
    return () => cancelAnimationFrame(raf);
  }, [focusId]);
}
