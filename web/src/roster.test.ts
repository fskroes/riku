import { describe, expect, it } from "vitest";
import type { SubAgent } from "./types";
import { rosterBadge, rosterRows } from "./roster";

// A minimal Sub-agent; only the fields a test exercises are given.
function subAgent(id: string, over: Partial<SubAgent> = {}): SubAgent {
  return {
    id,
    spawnKey: `toolu_${id}`,
    errand: `errand for ${id}`,
    state: "running",
    outcome: null,
    tokensIn: 0,
    tokensOut: 0,
    costUsd: null,
    model: null,
    depth: 0,
    lastEventAt: null,
    ...over,
  };
}

describe("rosterBadge", () => {
  it("shows nothing for a session that never fanned out", () => {
    // A badge that renders for everything says nothing when it is there.
    expect(rosterBadge([])).toBeNull();
  });

  it("counts only the running ones while any is running", () => {
    const badge = rosterBadge([
      subAgent("a"),
      subAgent("b", { state: "finished", outcome: "completed" }),
      subAgent("c"),
    ]);

    expect(badge).toEqual({ count: 2, running: true, label: "2 sub-agents running" });
  });

  it("falls back to the roster total once they have all finished", () => {
    // Not a zero and not an absence: the work a session delegated stays
    // discoverable after the fact.
    const badge = rosterBadge([
      subAgent("a", { state: "finished", outcome: "completed" }),
      subAgent("b", { state: "finished", outcome: "failed" }),
      subAgent("c", { state: "finished", outcome: "completed" }),
    ]);

    expect(badge).toEqual({
      count: 3,
      running: false,
      label: "3 sub-agents in all, none running",
    });
  });

  it("says which count it is showing, so the two states differ without colour or motion", () => {
    const live = rosterBadge([subAgent("a"), subAgent("b", { state: "finished" })]);
    const still = rosterBadge([
      subAgent("a", { state: "finished" }),
      subAgent("b", { state: "finished" }),
    ]);

    // Both pills read "1"/"2" — only the label distinguishes what the number means.
    expect(live?.label).toBe("1 sub-agent running");
    expect(still?.label).toBe("2 sub-agents in all, none running");
  });
});

describe("rosterRows", () => {
  it("keeps the order the Sub-agents were sent out", () => {
    // Reading top to bottom follows what the agent actually did, so a finished one
    // is not sorted away from its place in the fan-out.
    const rows = rosterRows([
      subAgent("first", { state: "finished", outcome: "completed" }),
      subAgent("second"),
      subAgent("third", { state: "finished", outcome: "failed" }),
    ]);

    // Keyed on the spawn key, the one identity that survives the join: a row the
    // parent alone established adopts the Sub-agent's own id once its file is
    // discovered, so keying on that would remount the row when its spend arrives.
    expect(rows.map((r) => r.id)).toEqual([
      "toolu_first",
      "toolu_second",
      "toolu_third",
    ]);
  });

  it("leaves a Sub-agent with no stated purpose unlabelled", () => {
    // A blank, never a placeholder: a label must not look like content when the
    // source named none.
    const [row] = rosterRows([subAgent("a", { errand: null })]);

    expect(row.errand).toBeNull();
  });

  it("carries the Errand verbatim", () => {
    const [row] = rosterRows([subAgent("a", { errand: "map the parser end to end" })]);

    expect(row.errand).toBe("map the parser end to end");
  });

  it("says how a finished Sub-agent ended, in the source's own word", () => {
    const rows = rosterRows([
      subAgent("a", { state: "finished", outcome: "failed" }),
      subAgent("b", { state: "finished", outcome: null }),
      subAgent("c"),
    ]);

    expect(rows.map((r) => r.state)).toEqual(["Finished · failed", "Finished", "Running"]);
    expect(rows.map((r) => r.running)).toEqual([false, false, true]);
  });

  it("ignores an outcome on a Sub-agent that is running again", () => {
    // A Sub-agent can resume after finishing, so the roster shows the latest word:
    // what it says now is that it is running.
    const [row] = rosterRows([subAgent("a", { state: "running", outcome: "completed" })]);

    expect(row.state).toBe("Running");
  });

  it("shows each Sub-agent's own spend, priced at its own model", () => {
    const [row] = rosterRows([
      subAgent("a", {
        tokensIn: 4700,
        tokensOut: 340,
        costUsd: 0.42,
        model: "claude-haiku-4-5",
      }),
    ]);

    expect(row.tokens).toBe("↑4.7k ↓340");
    expect(row.cost).toBe("$0.42");
    expect(row.model).toBe("Haiku 4.5");
  });

  it("shows no cost for an unpriced model rather than a zero", () => {
    const [row] = rosterRows([subAgent("a", { costUsd: null })]);

    expect(row.cost).toBeNull();
  });
});
