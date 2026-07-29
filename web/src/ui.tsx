import { useEffect, useState } from "react";
import { rosterBadge, rosterRows } from "./roster";
import type { DiffStat, Session, SubAgent, Tool } from "./types";
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

/** The Sub-agent fan-out badge and its roster panel (ADR 0014). Two states: a
 *  pulsing, accented pill counting what is running now; a still, dimmed pill
 *  carrying the roster total once they have all finished — the work a session
 *  delegated stays discoverable after the fact. Nothing at all when the roster is
 *  empty, so the badge means something when it is there.
 *
 *  The panel is the roster either way, one row per Sub-agent in spawn order. It
 *  renders here and never as expanded rows beneath a Band's own: a Band's count has
 *  to keep describing what is on screen (#64/#65). Which count, which label, and
 *  each row's text are `roster.ts`'s; this renders what it returns. */
export function SubAgentBadge({ roster }: { roster: SubAgent[] }) {
  const badge = rosterBadge(roster);
  if (!badge) return null;
  const rows = rosterRows(roster);
  return (
    <span className="subagents">
      <span
        className={badge.running ? "pill" : "pill still"}
        role="img"
        aria-label={badge.label}
        tabIndex={0}
      >
        {badge.running && <span className="pulse" aria-hidden="true" />}
        <span className="fan" aria-hidden="true">⑃</span> {badge.count}
      </span>
      <span className="tip" role="tooltip">
        <span className="tip-h">{badge.label}</span>
        <ul>
          {rows.map((row) => (
            <li key={row.id}>
              <span className={row.running ? "d" : "d done"} aria-hidden="true" />
              <span className="row">
                {/* A Sub-agent whose source named no purpose is unlabelled — the
                    Errand line is simply absent rather than filled with a stand-in. */}
                {row.errand && <span className="errand">{row.errand}</span>}
                <span className="sub">
                  <span className="st">{row.state}</span>
                  {row.model && <span className="k">{row.model}</span>}
                  <span className="tk">{row.tokens}</span>
                  {row.cost && <span className="ct">{row.cost}</span>}
                </span>
              </span>
            </li>
          ))}
        </ul>
      </span>
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
