/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
// The recap surface's own source, read as text — see the escaping test below.
import recapView from "./Recap.tsx?raw";
import journalModel from "./journal.ts?raw";
import type { CardJournal, CardResume, RecapCard, Session } from "./types";
import {
  ageLabel,
  byDay,
  dayLabel,
  handoffLabel,
  olderLine,
  resumeOffer,
  summaryLine,
  timelineFor,
  timelineMeta,
  whoLabel,
} from "./journal";

const resume = (over: Partial<CardResume> = {}): CardResume => ({
  instruction: "Pick the parser back up",
  command: null,
  dir: null,
  sessionGone: false,
  ...over,
});

const journal = (over: Partial<CardJournal> = {}): CardJournal => ({
  handoff: "on-track",
  next: "Wire the reader to the board",
  days: [],
  session: "s1",
  who: "agent",
  at: "2026-07-28T09:00:00Z",
  ageSeconds: 600,
  resume: resume(),
  ...over,
});

const card = (project: string, over: Partial<RecapCard> = {}): RecapCard => ({
  project,
  cwd: `/Users/x/${project}`,
  journal: journal(),
  ...over,
});

const session = (over: Partial<Session> = {}): Session => ({
  id: "s1",
  tool: "claude",
  project: "riku",
  model: null,
  branch: null,
  cwd: "/Users/x/riku",
  tokensIn: 0,
  tokensOut: 0,
  activity: null,
  lastEventAt: "2026-07-28T09:00:00Z",
  status: "active",
  attention: null,
  costUsd: null,
  diff: null,
  subAgents: { active: 0, descriptions: [] },
  machine: null,
  ...over,
});

describe("Handoff Status labels", () => {
  it("spells each status as prose, never the raw wire word", () => {
    expect(handoffLabel("needs-you")).toBe("Needs you");
    expect(handoffLabel("needs-review")).toBe("Needs review");
    expect(handoffLabel("on-track")).toBe("On track");
  });

  it("names the voice that had the last word", () => {
    expect(whoLabel("agent")).toBe("agent suggests");
    expect(whoLabel("user")).toBe("your call");
  });
});

describe("ageLabel", () => {
  it("labels entry age so a stale next step is visible", () => {
    expect(ageLabel(7200)).toBe("latest 2h ago");
    expect(ageLabel(180)).toBe("latest 3m ago");
    expect(ageLabel(172800)).toBe("latest 2d ago");
  });

  it("reads as just now under a minute, not '0s ago'", () => {
    expect(ageLabel(0)).toBe("latest just now");
    expect(ageLabel(41)).toBe("latest just now");
  });

  it("never reads negative for an entry stamped in the future", () => {
    expect(ageLabel(-90)).toBe("latest just now");
  });
});

describe("timelineFor", () => {
  const sessions = [
    session({ id: "old", lastEventAt: "2026-07-28T08:00:00Z", activity: "Reading journal.rs" }),
    session({ id: "new", lastEventAt: "2026-07-28T10:00:00Z", activity: "Editing recap.rs" }),
    session({ id: "elsewhere", cwd: "/Users/x/other", lastEventAt: "2026-07-28T11:00:00Z" }),
    session({ id: "nowhere", cwd: null, lastEventAt: "2026-07-28T12:00:00Z" }),
  ];

  it("keeps only the project's own sessions, newest first", () => {
    expect(timelineFor(sessions, "/Users/x/riku").map((r) => r.id)).toEqual(["new", "old"]);
  });

  it("carries the derived activity as ground truth beside the prose", () => {
    const [first] = timelineFor(sessions, "/Users/x/riku");
    expect(first.activity).toBe("Editing recap.rs");
    expect(first.status).toBe("active");
  });

  it("is empty for a project with no known sessions", () => {
    expect(timelineFor(sessions, "/Users/x/gone")).toEqual([]);
  });
});

describe("timelineMeta", () => {
  const now = Date.parse("2026-07-28T12:00:00Z");

  it("counts the sessions behind the timeline and dates the last event", () => {
    const rows = timelineFor([session({ lastEventAt: "2026-07-28T10:00:00Z" })], "/Users/x/riku");
    expect(timelineMeta(rows, now)).toBe("1 session · last 2h ago");
  });

  it("says so plainly when no transcript is left to show", () => {
    expect(timelineMeta([], now)).toBe("no sessions in the last 24h");
  });
});

describe("resumeOffer", () => {
  it("offers the command Riku built, with the directory it belongs in", () => {
    const offer = resumeOffer(resume({ command: "claude --resume s1", dir: "/Users/x/riku" }));
    expect(offer).toEqual({
      kind: "command",
      command: "claude --resume s1",
      dir: "/Users/x/riku",
      instruction: "Pick the parser back up",
    });
  });

  it("falls back to the instruction with a session-gone note", () => {
    const offer = resumeOffer(resume({ sessionGone: true }));
    expect(offer).toEqual({ kind: "gone", instruction: "Pick the parser back up" });
  });

  it("shows the instruction alone when the entry named no session", () => {
    expect(resumeOffer(resume())).toEqual({ kind: "instruction", instruction: "Pick the parser back up" });
  });

  it("withholds a command that arrives beside a session-gone marker", () => {
    // Belt and braces: the endpoint never sends both, and a card must not paste
    // a command for a thread it has just called unreachable.
    const offer = resumeOffer(resume({ command: "claude --resume s1", sessionGone: true }));
    expect(offer.kind).toBe("gone");
  });
});

describe("byDay", () => {
  const cards: RecapCard[] = [
    card("riku", {
      journal: journal({
        handoff: "needs-you",
        days: [
          { date: "2026-07-28", done: ["Served the recap"] },
          { date: "2026-07-27", done: ["Wrote the reader", "Wired the hook"] },
        ],
      }),
    }),
    card("ledger", {
      journal: journal({ days: [{ date: "2026-07-27", done: ["Capped the file"] }] }),
    }),
    card("quiet", { journal: null }),
  ];

  it("groups every project's done lines under the day they were reported", () => {
    const days = byDay(cards);
    expect(days.map((d) => d.date)).toEqual(["2026-07-28", "2026-07-27"]);
    expect(days[1].rows.map((r) => r.project)).toEqual(["riku", "ledger"]);
    expect(days[1].rows[0].done).toEqual(["Wrote the reader", "Wired the hook"]);
  });

  it("carries each row's Handoff Status and cwd so a day row leads back to its thread", () => {
    const [today] = byDay(cards);
    expect(today.rows[0]).toMatchObject({ project: "riku", cwd: "/Users/x/riku", handoff: "needs-you" });
  });

  it("leaves out cards with no journal — the day view invents nothing", () => {
    expect(byDay(cards).flatMap((d) => d.rows.map((r) => r.project))).not.toContain("quiet");
    expect(byDay([card("quiet", { journal: null })])).toEqual([]);
  });
});

describe("dayLabel", () => {
  // Local dates, so the labels are the reader's own day (matching the backend's
  // local-date grouping) rather than UTC's.
  const now = new Date(2026, 6, 28, 14, 0, 0).getTime();

  it("names today and yesterday", () => {
    expect(dayLabel("2026-07-28", now)).toBe("Today");
    expect(dayLabel("2026-07-27", now)).toBe("Yesterday");
  });

  it("dates anything older", () => {
    expect(dayLabel("2026-07-24", now)).toBe("Fri 24 Jul");
  });
});

describe("summaryLine", () => {
  it("leads with how many threads want a human", () => {
    const cards = [
      card("a", { journal: journal({ handoff: "needs-you" }) }),
      card("b", { journal: journal({ handoff: "needs-review" }) }),
      card("c", { journal: journal({ handoff: "on-track" }) }),
    ];
    expect(summaryLine(cards, true)).toBe("3 threads · 2 want you — they are on top.");
  });

  it("agrees with a single waiting thread", () => {
    const cards = [card("a", { journal: journal({ handoff: "needs-you" }) }), card("b")];
    expect(summaryLine(cards, true)).toBe("2 threads · 1 wants you — it is on top.");
  });

  it("says the desk is clear when nothing wants a human", () => {
    expect(summaryLine([card("a")], true)).toBe("1 thread · nothing is waiting on you.");
  });

  it("says nothing is recorded when there are no threads at all", () => {
    expect(summaryLine([], true)).toBe("No threads yet.");
  });

  it("never claims calm from files it did not read", () => {
    // Journal off: every card arrives without prose, and "nothing is waiting on
    // you" would be a verdict on unread files.
    expect(summaryLine([card("a", { journal: null })], false)).toBe("1 thread · the journal is off.");
    expect(summaryLine([], false)).toBe("The journal is off.");
  });
});

describe("olderLine", () => {
  it("reports the cap rather than implying the list is everything", () => {
    expect(olderLine(5, 12)).toBe("5 of 12 journals with no recent session");
    expect(olderLine(3, 3)).toBe("3 journals with no recent session");
    expect(olderLine(1, 1)).toBe("1 journal with no recent session");
  });
});

describe("journal prose is data, never markup", () => {
  // ADR 0013: Riku renders text it did not produce. React escapes text nodes, so
  // the one way a card could interpret an entry is an explicit HTML escape hatch
  // — there must not be one anywhere on the recap surface.
  it("has no HTML escape hatch on the recap surface", () => {
    for (const source of [recapView, journalModel]) {
      expect(source).not.toMatch(/dangerouslySetInnerHTML|innerHTML/);
    }
  });
});
