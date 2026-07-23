import { useEffect, useState } from "react";
import type { DiffStat, Session, SubAgents, Tool } from "./types";
import { abbrevTokens, diffLabel, formatCost, shortModel, tokensLabel } from "./format";

/** Per-tool tile glyph, accessible name + accent. One Session Source per tool
 *  (issue #5). `name` is the screen-reader label so the tile is not announced as
 *  a bare "C"/"◆" glyph (audit M4). */
export const TOOL: Record<Tool, { label: string; name: string; color: string }> = {
  claude: { label: "C", name: "Claude Code", color: "#E8590C" },
  codex: { label: "◆", name: "Codex", color: "#10A37F" },
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
  const { label, name, color } = TOOL[tool];
  return (
    <span
      className={small ? "tile sm" : "tile"}
      style={{ ["--tile" as string]: color }}
      role="img"
      aria-label={name}
    >
      <span aria-hidden="true">{label}</span>
    </span>
  );
}

/** The machine chip (C7): which machine an Agent Session is on. Shown on every
 *  card so a mixed local + Relay board labels sessions consistently. Renders
 *  nothing for an unstamped (pre-C7) session. */
export function Machine({ host }: { host: string | null }) {
  if (!host) return null;
  return (
    <span className="machine" role="img" aria-label={`Running on ${host}`} title={`Running on ${host}`}>
      <span className="mdot" aria-hidden="true" /> {host}
    </span>
  );
}

/** The git branch chip shared by every session presentation (C5). The `⑂` is
 *  decorative and hidden; the chip announces as a labelled "branch <name>" and
 *  the full name is `title`-backed so a clipped branch stays recoverable (audit
 *  M2/M4). `className` places it in each surface (`.rbranch`, `.branch`, or the
 *  Work Link `.sub`). Renders nothing for a branchless session. */
export function Branch({ branch, className }: { branch: string | null; className?: string }) {
  if (!branch) return null;
  return (
    <span className={className} role="img" aria-label={`branch ${branch}`} title={branch}>
      <span aria-hidden="true">⑂ </span>
      {branch}
    </span>
  );
}

/** The `↑in/out` token-count stat shared by session cards. The `↑` is decorative
 *  and hidden; the pair announces as "N tokens in, N tokens out" rather than
 *  literal glyphs (audit M4). `spaced` sets the roomier `↑ in / out` form used in
 *  the metadata line vs the tight form used in a compact row. */
export function Tokens({ tokensIn, tokensOut, spaced }: { tokensIn: number; tokensOut: number; spaced?: boolean }) {
  const sep = spaced ? " / " : "/";
  return (
    <span role="img" aria-label={tokensLabel(tokensIn, tokensOut)}>
      <span aria-hidden="true">↑</span>
      {spaced ? " " : ""}
      {abbrevTokens(tokensIn)}
      {sep}
      {abbrevTokens(tokensOut)}
    </span>
  );
}

/** The git `+/-` stat for a card (C5). Renders nothing when the session has no
 *  repo diff (e.g. its cwd is not a git checkout). */
export function Diff({ diff }: { diff: DiffStat | null }) {
  if (!diff) return null;
  return (
    <span
      className="diff"
      role="img"
      aria-label={diffLabel(diff)}
      title="Lines changed on this branch (uncommitted + since default branch)"
    >
      <span className="add" aria-hidden="true">
        +{diff.added}
      </span>
      <span className="del" aria-hidden="true">
        −{diff.removed}
      </span>
    </span>
  );
}

/** The Sub-agent fan-out badge (issue #23): a count pill on the parent's card
 *  showing how many Sub-agents are running right now, with each active one's short
 *  description revealed on hover/focus. A Sub-agent is never its own card. Renders
 *  nothing when none is active — a quiet or Codex session stays badge-less. The
 *  count is the accessible label; the pulse dot is decorative "still working". */
export function SubAgentBadge({ subAgents }: { subAgents: SubAgents }) {
  const { active, descriptions } = subAgents;
  if (active <= 0) return null;
  const noun = active === 1 ? "sub-agent" : "sub-agents";
  return (
    <span className="subagents">
      <span
        className="pill"
        role="img"
        aria-label={`${active} ${noun} running`}
        // The full list rides `title` too, so the descriptions are reachable without
        // hover (native tooltip) as well as through the styled panel below.
        title={descriptions.length ? descriptions.join("\n") : undefined}
        tabIndex={0}
      >
        <span className="pulse" aria-hidden="true" />
        <span className="fan" aria-hidden="true">⑃</span> {active}
      </span>
      {descriptions.length > 0 && (
        <span className="tip" role="tooltip">
          <span className="tip-h">{active} {noun} running</span>
          <ul>
            {descriptions.map((d, i) => (
              <li key={i}>
                <span className="d" aria-hidden="true" />
                {d}
              </li>
            ))}
          </ul>
        </span>
      )}
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
      <Branch branch={session.branch} />
      <Tokens tokensIn={session.tokensIn} tokensOut={session.tokensOut} spaced />
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
