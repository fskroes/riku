import { describe, expect, it } from "vitest";
import type { Session, Status } from "./types";
import { finishedBand, finishedLine } from "./bands";

// A minimal Agent Session; only the band-relevant fields matter here. `lastEventAt`
// is given as a plain hour on a fixed day, so a fixture's recency is readable.
function session(
  id: string,
  hour: number,
  status: Status = "finished",
  over: Partial<Session> = {},
): Session {
  return {
    id,
    tool: "claude",
    project: "p",
    model: null,
    branch: null,
    cwd: null,
    tokensIn: 0,
    tokensOut: 0,
    activity: null,
    lastEventAt: `2026-07-28T${String(hour).padStart(2, "0")}:00:00Z`,
    status,
    attention: null,
    costUsd: null,
    diff: null,
    subAgents: { active: 0, descriptions: [] },
    machine: null,
    ...over,
  };
}

describe("finishedBand", () => {
  it("shows only the five most recent, newest first", () => {
    // Seven Finished sessions, handed in oldest-first so the result cannot be
    // the input order surviving untouched.
    const sessions = [1, 2, 3, 4, 5, 6, 7].map((h) => session(`s${h}`, h));

    const band = finishedBand(sessions, null);

    expect(band.shown.map((s) => s.id)).toEqual(["s7", "s6", "s5", "s4", "s3"]);
  });

  it("counts every Finished session, so the cap can be disclosed", () => {
    const sessions = [
      ...[1, 2, 3, 4, 5, 6, 7].map((h) => session(`s${h}`, h)),
      // Neither of these is Finished, so neither belongs to this band's total.
      session("running", 8, "active"),
      session("waiting", 9, "attention"),
    ];

    expect(finishedBand(sessions, null).total).toBe(7);
  });

  it("keeps a focused session the cap would otherwise have hidden", () => {
    // s1 is the oldest of seven, so the cap alone would drop it. Arriving from a
    // Work Item's session chip, it is the one card the reader came to see.
    const sessions = [1, 2, 3, 4, 5, 6, 7].map((h) => session(`s${h}`, h));

    const band = finishedBand(sessions, "s1");

    expect(band.shown.map((s) => s.id)).toEqual(["s7", "s6", "s5", "s4", "s3", "s1"]);
    expect(band.total).toBe(7);
  });

  it("does not list a focused session twice when the cap already kept it", () => {
    const sessions = [1, 2, 3, 4, 5, 6, 7].map((h) => session(`s${h}`, h));

    expect(finishedBand(sessions, "s7").shown.map((s) => s.id)).toEqual([
      "s7",
      "s6",
      "s5",
      "s4",
      "s3",
    ]);
  });

  it("does not pull a focused session into the band from another band", () => {
    // The focus is a Running session; it belongs above, and the exemption must not
    // drag it down here.
    const sessions = [
      ...[1, 2, 3, 4, 5, 6, 7].map((h) => session(`s${h}`, h)),
      session("running", 8, "active"),
    ];

    const band = finishedBand(sessions, "running");

    expect(band.shown.map((s) => s.id)).toEqual(["s7", "s6", "s5", "s4", "s3"]);
    expect(band.total).toBe(7);
  });
});

describe("finishedLine", () => {
  it("reports the cap rather than implying the list is everything", () => {
    expect(finishedLine(5, 12)).toBe("5 of 12");
  });

  it("stays a bare count when nothing is hidden", () => {
    // The band's own label already says "Finished", so an uncapped band reads
    // exactly as it did before the cap existed.
    expect(finishedLine(3, 3)).toBe("3");
    expect(finishedLine(0, 0)).toBe("0");
  });

  it("stays a bare count when a focused session pushes past the cap", () => {
    // 6 shown of 6 total: the focus exemption is not a hidden remainder, so there
    // is nothing to disclose.
    expect(finishedLine(6, 6)).toBe("6");
  });
});
