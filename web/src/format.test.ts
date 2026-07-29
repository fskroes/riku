import { describe, expect, it } from "vitest";
import { columnLabel, sourceStatusNote } from "./format";

describe("sourceStatusNote", () => {
  it("names what the source still says when a live Work Link raised the status", () => {
    // The #66 card: the board shows In progress because an agent is on it, while
    // nobody has written the marker.
    expect(sourceStatusNote("doing", "todo", "workMd")).toBe("To do in WORK.md");
    expect(sourceStatusNote("doing", "todo", "github")).toBe("To do in GitHub Issues");
  });

  it("stays quiet when the board and the source agree", () => {
    // Nothing was raised, so there is no difference to disclose and the card reads
    // exactly as it did before the derivation existed.
    expect(sourceStatusNote("todo", "todo", "workMd")).toBeNull();
    expect(sourceStatusNote("doing", "doing", "workMd")).toBeNull();
    expect(sourceStatusNote("done", "done", "github")).toBeNull();
  });

});

describe("columnLabel", () => {
  it("reads as the kanban's own column heads do", () => {
    expect(columnLabel("todo")).toBe("To do");
    expect(columnLabel("doing")).toBe("In progress");
    expect(columnLabel("done")).toBe("Done");
  });
});
