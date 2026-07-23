// The dependency-graph model (ADR 0005), extracted from the view so the layout
// and — for #37 — the non-visual reading of the graph are pure, testable
// functions rather than JSX-bound logic.

import type { LinkedSession, WorkItem, WorkStatus } from "./types";

const STATUS_LABEL: Record<WorkStatus, string> = {
  todo: "to do",
  doing: "in progress",
  done: "done",
};

/** A Work Item status as prose ("to do", "in progress", "done") — the one spoken
 *  form, shared by the graph node's name, the ordered reading, and the legend so a
 *  screen reader never hears "to do" in one place and "todo" in another. */
export function statusLabel(status: WorkStatus): string {
  return STATUS_LABEL[status];
}

/** A linked Agent Session's status as the graph's spoken phrase, shared by the
 *  visual node's name and the ordered reading so both describe the agent the same
 *  way. Distinct from the visible pill glyph (see WorkItems' agentPillText). */
export function agentAccessibleText(status: LinkedSession["status"]): string {
  return status === "attention" ? "linked agent needs attention" : status === "finished" ? "linked agent idle" : "agent working";
}

/** Longest-path depth (graph column) per item, over in-set blocked-by edges.
 *  Cycles resolve to depth 0 rather than looping. */
export function computeDepths(items: WorkItem[]): Map<string, number> {
  const byId = new Map(items.map((i) => [i.id, i]));
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const d = (id: string): number => {
    const memo = depth.get(id);
    if (memo !== undefined) return memo;
    if (visiting.has(id)) return 0; // cycle guard
    visiting.add(id);
    const blockers = (byId.get(id)?.blockedBy ?? []).filter((b) => byId.has(b));
    const val = blockers.length ? 1 + Math.max(...blockers.map(d)) : 0;
    visiting.delete(id);
    depth.set(id, val);
    return val;
  };
  items.forEach((i) => d(i.id));
  return depth;
}

export interface GraphModel {
  edges: { from: string; to: string }[];
  blockers: Map<string, string[]>; // id -> in-set ids it is blocked by
  dependents: Map<string, string[]>; // id -> in-set ids blocked by it
  criticalNodes: Set<string>;
  criticalEdges: Set<string>; // "from->to" keys along the critical path
}

/** Blocker/dependent adjacency, the blocker→dependent edge list, and the critical
 *  path: the single longest dependency chain, so it can be drawn as a spine. Uses
 *  only in-set edges and is cycle-guarded (matches computeDepths' tolerance). */
export function buildGraphModel(items: WorkItem[], depth: Map<string, number>): GraphModel {
  const byId = new Map(items.map((i) => [i.id, i]));
  const blockers = new Map<string, string[]>();
  const dependents = new Map<string, string[]>();
  for (const it of items) dependents.set(it.id, []);
  for (const it of items) {
    const bs = it.blockedBy.filter((b) => byId.has(b));
    blockers.set(it.id, bs);
    for (const b of bs) dependents.get(b)!.push(it.id);
  }
  const edges: { from: string; to: string }[] = [];
  for (const it of items) for (const b of blockers.get(it.id)!) edges.push({ from: b, to: it.id });

  // Longest downstream chain from each node (by node count), cycle-guarded.
  const best = new Map<string, { len: number; next: string | null }>();
  const visiting = new Set<string>();
  const longest = (id: string): { len: number; next: string | null } => {
    const memo = best.get(id);
    if (memo) return memo;
    if (visiting.has(id)) return { len: 1, next: null };
    visiting.add(id);
    let pick: { len: number; next: string | null } = { len: 1, next: null };
    for (const dep of dependents.get(id)!) {
      const sub = longest(dep);
      if (sub.len + 1 > pick.len) pick = { len: sub.len + 1, next: dep };
    }
    visiting.delete(id);
    best.set(id, pick);
    return pick;
  };
  items.forEach((i) => longest(i.id));

  // The critical path starts at the root (depth 0) with the longest chain.
  let start: string | null = null;
  let bestLen = 0;
  for (const it of items) {
    if ((depth.get(it.id) ?? 0) !== 0) continue;
    const len = best.get(it.id)!.len;
    if (len > bestLen) {
      bestLen = len;
      start = it.id;
    }
  }
  const criticalNodes = new Set<string>();
  const criticalEdges = new Set<string>();
  for (let cur = start; cur; cur = best.get(cur)!.next) {
    criticalNodes.add(cur);
    const nxt = best.get(cur)!.next;
    if (nxt) criticalEdges.add(`${cur}->${nxt}`);
  }
  return { edges, blockers, dependents, criticalNodes, criticalEdges };
}

/** The lineage of `id`: itself plus every ancestor (up the blocked-by chain) and
 *  every descendant (down the dependents chain) — what hovering or focusing a node
 *  highlights. */
export function lineageOf(model: GraphModel, id: string): Set<string> {
  const set = new Set<string>([id]);
  const up = (x: string): void => {
    for (const b of model.blockers.get(x) ?? []) if (!set.has(b)) (set.add(b), up(b));
  };
  const down = (x: string): void => {
    for (const dep of model.dependents.get(x) ?? []) if (!set.has(dep)) (set.add(dep), down(dep));
  };
  up(id);
  down(id);
  return set;
}

/** A stable dependency reading order: blockers before the things they block
 *  (depth ascending), ties broken by the item's original position. This is the
 *  order the non-visual ordered fallback and the keyboard tab order follow, so a
 *  screen-reader user meets a node only after the nodes it depends on (#37 H4). */
export function readingOrder(items: WorkItem[], depth: Map<string, number>): WorkItem[] {
  return items
    .map((item, index) => ({ item, index }))
    .sort((a, b) => {
      const da = depth.get(a.item.id) ?? 0;
      const db = depth.get(b.item.id) ?? 0;
      return da - db || a.index - b.index;
    })
    .map((e) => e.item);
}

/** The blocked-by relation as words: "Blocked by W-1, W-2", or "" when the item
 *  is a root. Ids only — kept short so it reads cleanly inside a node's name. */
export function blockedByLabel(model: GraphModel, id: string): string {
  const bs = model.blockers.get(id) ?? [];
  return bs.length ? `Blocked by ${bs.join(", ")}` : "";
}

/** The dependents relation as words: "Blocks W-5, W-6", or "" for a leaf. */
export function blocksLabel(model: GraphModel, id: string): string {
  const ds = model.dependents.get(id) ?? [];
  return ds.length ? `Blocks ${ds.join(", ")}` : "";
}

/** The full accessible name for one graph node: identity, status, critical-path
 *  membership, dependency relations, and any linked Agent Session — everything the
 *  visual node conveys through position, colour, and glyphs, said in words so a
 *  non-visual user gets the same facts (#37 H4). */
export function nodeAccessibleName(item: WorkItem, model: GraphModel): string {
  const parts = [`${item.id}: ${item.title}`, STATUS_LABEL[item.status]];
  if (model.criticalNodes.has(item.id)) parts.push("on the critical path");
  const blocked = blockedByLabel(model, item.id);
  if (blocked) parts.push(blocked);
  const blocks = blocksLabel(model, item.id);
  if (blocks) parts.push(blocks);
  if (item.session) parts.push(agentAccessibleText(item.session.status));
  return parts.join(". ") + ".";
}
