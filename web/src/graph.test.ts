import { describe, expect, it } from "vitest";
import type { LinkedSession, WorkItem } from "./types";
import {
  agentAccessibleText,
  blockedByLabel,
  blocksLabel,
  buildGraphModel,
  computeDepths,
  lineageOf,
  nodeAccessibleName,
  readingOrder,
  statusLabel,
} from "./graph";

// A minimal Work Item; only the graph-relevant fields matter here.
function item(id: string, blockedBy: string[], over: Partial<WorkItem> = {}): WorkItem {
  return { id, title: `${id} title`, status: "todo", effort: null, blockedBy, session: null, ...over };
}

const session = (over: Partial<LinkedSession> = {}): LinkedSession => ({
  id: "s1",
  project: "p",
  tool: "claude",
  model: null,
  branch: null,
  status: "active",
  machine: null,
  ...over,
});

// A → B → C chain, plus D which A also blocks (a side branch off the spine).
const CHAIN: WorkItem[] = [
  item("A", []),
  item("B", ["A"]),
  item("C", ["B"]),
  item("D", ["A"]),
];

describe("computeDepths", () => {
  it("assigns longest-path depth over blocked-by edges", () => {
    const d = computeDepths(CHAIN);
    expect(d.get("A")).toBe(0);
    expect(d.get("B")).toBe(1);
    expect(d.get("C")).toBe(2);
    expect(d.get("D")).toBe(1);
  });

  it("resolves a cycle to depth 0 rather than looping", () => {
    const cyclic = [item("X", ["Y"]), item("Y", ["X"])];
    const d = computeDepths(cyclic);
    expect(d.get("X")).toBeDefined();
    expect(d.get("Y")).toBeDefined();
  });

  it("ignores blocked-by ids that are not in the set", () => {
    const d = computeDepths([item("A", ["ghost"])]);
    expect(d.get("A")).toBe(0);
  });
});

describe("buildGraphModel", () => {
  const model = buildGraphModel(CHAIN, computeDepths(CHAIN));

  it("records blockers and dependents both ways", () => {
    expect(model.blockers.get("B")).toEqual(["A"]);
    expect(model.dependents.get("A")!.sort()).toEqual(["B", "D"]);
    expect(model.dependents.get("C")).toEqual([]);
  });

  it("marks the longest chain as the critical path", () => {
    expect([...model.criticalNodes].sort()).toEqual(["A", "B", "C"]);
    expect(model.criticalEdges.has("A->B")).toBe(true);
    expect(model.criticalEdges.has("B->C")).toBe(true);
    expect(model.criticalEdges.has("A->D")).toBe(false);
  });
});

describe("lineageOf", () => {
  it("collects ancestors and descendants of a node", () => {
    const model = buildGraphModel(CHAIN, computeDepths(CHAIN));
    expect([...lineageOf(model, "B")].sort()).toEqual(["A", "B", "C"]);
  });
});

describe("readingOrder", () => {
  it("orders blockers before the items they block (depth ascending)", () => {
    const shuffled = [item("C", ["B"]), item("A", []), item("B", ["A"])];
    const order = readingOrder(shuffled, computeDepths(shuffled)).map((i) => i.id);
    expect(order).toEqual(["A", "B", "C"]);
  });

  it("breaks depth ties by original position (stable)", () => {
    const order = readingOrder(CHAIN, computeDepths(CHAIN)).map((i) => i.id);
    // B and D are both depth 1; B precedes D in the input so it precedes in output.
    expect(order).toEqual(["A", "B", "D", "C"]);
  });
});

describe("dependency labels", () => {
  const model = buildGraphModel(CHAIN, computeDepths(CHAIN));

  it("names blockers, or empty for a root", () => {
    expect(blockedByLabel(model, "C")).toBe("Blocked by B");
    expect(blockedByLabel(model, "A")).toBe("");
  });

  it("names dependents, or empty for a leaf", () => {
    expect(blocksLabel(model, "A")).toBe("Blocks B, D");
    expect(blocksLabel(model, "C")).toBe("");
  });
});

describe("shared spoken labels", () => {
  it("renders a WorkStatus as prose (todo → 'to do', never raw)", () => {
    expect(statusLabel("todo")).toBe("to do");
    expect(statusLabel("doing")).toBe("in progress");
    expect(statusLabel("done")).toBe("done");
  });

  it("renders a linked agent's status as a spoken phrase", () => {
    expect(agentAccessibleText("attention")).toBe("linked agent needs attention");
    expect(agentAccessibleText("finished")).toBe("linked agent idle");
    expect(agentAccessibleText("active")).toBe("agent working");
  });
});

describe("nodeAccessibleName", () => {
  const model = buildGraphModel(CHAIN, computeDepths(CHAIN));

  it("states identity, status, critical path, and both relations", () => {
    const name = nodeAccessibleName(item("B", ["A"]), model);
    expect(name).toBe("B: B title. to do. on the critical path. Blocked by A. Blocks C.");
  });

  it("omits critical/relations that do not apply and includes agent state", () => {
    const leaf = item("D", ["A"], { status: "doing", session: session({ status: "attention" }) });
    const name = nodeAccessibleName(leaf, model);
    expect(name).toBe("D: D title. in progress. Blocked by A. linked agent needs attention.");
  });
});
