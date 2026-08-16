import { describe, expect, it } from "vitest";

import type { ActionItem, Meeting, Ticket } from "./api";
import {
  durationMinutes,
  isOpen,
  isOverdue,
  loadByOwner,
  meetingsPerDay,
  readableMinutes,
  totals,
} from "./metrics";

const NOW = new Date("2026-08-16T12:00:00");

function meeting(started: string, ended: string | null = null): Meeting {
  return {
    id: Math.random().toString(36).slice(2),
    project_id: null,
    title: "Meeting",
    source: "import",
    started_at: started,
    ended_at: ended,
  };
}

function item(patch: Partial<ActionItem> = {}): ActionItem {
  return {
    id: Math.random().toString(36).slice(2),
    text: "do the thing",
    owner: null,
    due_at: null,
    status: "todo",
    ...patch,
  };
}

describe("isOpen", () => {
  it("counts anything not finished or abandoned", () => {
    expect(isOpen({ status: "todo" })).toBe(true);
    expect(isOpen({ status: "in_progress" })).toBe(true);
    expect(isOpen({ status: "done" })).toBe(false);
    expect(isOpen({ status: "cancelled" })).toBe(false);
  });

  // The summarize endpoint returns items without one before they have been read back.
  it("treats a missing status as open", () => {
    expect(isOpen({})).toBe(true);
  });
});

describe("isOverdue", () => {
  it("is true only for open work past its date", () => {
    expect(isOverdue(item({ due_at: "2026-08-01T00:00:00Z" }), NOW)).toBe(true);
    expect(isOverdue(item({ due_at: "2026-09-01T00:00:00Z" }), NOW)).toBe(false);
  });

  // Finishing something late does not leave it overdue forever.
  it("is false once the work is done", () => {
    expect(isOverdue(item({ due_at: "2026-08-01T00:00:00Z", status: "done" }), NOW)).toBe(false);
  });

  it("is false without a date", () => {
    expect(isOverdue(item({ due_at: null }), NOW)).toBe(false);
  });

  it("does not throw on a malformed date", () => {
    expect(isOverdue(item({ due_at: "not a date" }), NOW)).toBe(false);
  });
});

describe("durationMinutes", () => {
  it("measures a finished meeting", () => {
    expect(durationMinutes(meeting("2026-08-16T09:00:00Z", "2026-08-16T09:45:00Z"))).toBe(45);
  });

  // A meeting left open by a crash would otherwise report the time since it started, which
  // grows forever and quietly inflates every total on the page.
  it("counts an unfinished meeting as zero", () => {
    expect(durationMinutes(meeting("2026-08-16T09:00:00Z", null))).toBe(0);
  });

  it("refuses to return a negative duration", () => {
    expect(durationMinutes(meeting("2026-08-16T10:00:00Z", "2026-08-16T09:00:00Z"))).toBe(0);
  });

  it("does not throw on malformed timestamps", () => {
    expect(durationMinutes(meeting("nonsense", "also nonsense"))).toBe(0);
  });
});

describe("totals", () => {
  it("adds up what is there", () => {
    const result = totals({
      meetings: [
        meeting("2026-08-16T09:00:00Z", "2026-08-16T09:30:00Z"),
        meeting("2026-08-15T09:00:00Z", "2026-08-15T10:00:00Z"),
      ],
      notes: [
        { id: "n1", title: "a", body: "", created_at: "", updated_at: "", deleted_at: null },
      ],
      actionItems: [item(), item({ status: "done" })],
      tickets: [],
      decisions: 4,
    });

    expect(result).toEqual({
      meetings: 2,
      minutes: 90,
      notes: 1,
      openWork: 1,
      overdue: 0,
      decisions: 4,
    });
  });

  it("counts action items and tickets as one pool of work", () => {
    const ticket: Ticket = {
      id: "t1",
      title: "fix it",
      description: null,
      status: "todo",
      owner: null,
      due_at: null,
    };

    const result = totals({
      meetings: [],
      notes: [],
      actionItems: [item()],
      tickets: [ticket],
      decisions: 0,
    });
    expect(result.openWork).toBe(2);
  });

  it("reports zeroes for an empty workspace rather than throwing", () => {
    const result = totals({
      meetings: [],
      notes: [],
      actionItems: [],
      tickets: [],
      decisions: 0,
    });
    expect(result.meetings).toBe(0);
    expect(result.minutes).toBe(0);
  });
});

describe("meetingsPerDay", () => {
  it("returns one entry per day in the window, oldest first", () => {
    const days = meetingsPerDay([], 7, NOW);
    expect(days).toHaveLength(7);
    expect(days[0].day < days[6].day).toBe(true);
    expect(days[6].day).toBe("2026-08-16");
  });

  // The gaps are the information: omitting quiet days turns a fortnight of nothing into a
  // flat busy line.
  it("keeps empty days", () => {
    const days = meetingsPerDay([meeting("2026-08-16T09:00:00")], 5, NOW);
    expect(days.filter((d) => d.count === 0)).toHaveLength(4);
  });

  it("counts several meetings on the same day", () => {
    const days = meetingsPerDay(
      [
        meeting("2026-08-16T09:00:00"),
        meeting("2026-08-16T14:00:00"),
        meeting("2026-08-15T09:00:00"),
      ],
      5,
      NOW,
    );
    expect(days.at(-1)).toEqual({ day: "2026-08-16", count: 2 });
    expect(days.at(-2)).toEqual({ day: "2026-08-15", count: 1 });
  });

  it("ignores meetings older than the window", () => {
    const days = meetingsPerDay([meeting("2020-01-01T09:00:00")], 7, NOW);
    expect(days.every((d) => d.count === 0)).toBe(true);
  });

  it("does not throw on a malformed timestamp", () => {
    expect(() => meetingsPerDay([meeting("nonsense")], 7, NOW)).not.toThrow();
  });

  // A meeting at 9pm belongs to the day the person was in it, whatever that is in UTC.
  it("buckets by local date", () => {
    const late = meeting("2026-08-16T21:30:00");
    const days = meetingsPerDay([late], 3, NOW);
    expect(days.at(-1)).toEqual({ day: "2026-08-16", count: 1 });
  });
});

describe("loadByOwner", () => {
  it("groups open work and sorts by volume", () => {
    const load = loadByOwner([
      item({ owner: "Dana" }),
      item({ owner: "Dana" }),
      item({ owner: "Sam" }),
    ]);
    expect(load).toEqual([
      { owner: "Dana", open: 2, overdue: 0 },
      { owner: "Sam", open: 1, overdue: 0 },
    ]);
  });

  it("excludes finished work", () => {
    const load = loadByOwner([item({ owner: "Dana", status: "done" }), item({ owner: "Sam" })]);
    expect(load).toEqual([{ owner: "Sam", open: 1, overdue: 0 }]);
  });

  // Usually the biggest row, and the one that matters: an item nobody owns is the one that
  // does not get done.
  it("keeps unowned work as its own row", () => {
    const load = loadByOwner([item(), item({ owner: "   " }), item({ owner: "Dana" })]);
    expect(load[0]).toEqual({ owner: "Unassigned", open: 2, overdue: 0 });
  });

  it("counts overdue within each owner", () => {
    const load = loadByOwner([
      item({ owner: "Dana", due_at: "2020-01-01T00:00:00Z" }),
      item({ owner: "Dana" }),
    ]);
    expect(load[0]).toEqual({ owner: "Dana", open: 2, overdue: 1 });
  });

  it("breaks a tie on volume with overdue, then alphabetically", () => {
    const load = loadByOwner([
      item({ owner: "Sam" }),
      item({ owner: "Dana", due_at: "2020-01-01T00:00:00Z" }),
    ]);
    expect(load[0].owner).toBe("Dana");
  });

  it("returns nothing for an empty list", () => {
    expect(loadByOwner([])).toEqual([]);
  });
});

describe("readableMinutes", () => {
  it("reads minutes, hours, and both", () => {
    expect(readableMinutes(0)).toBe("0m");
    expect(readableMinutes(45)).toBe("45m");
    expect(readableMinutes(60)).toBe("1h");
    expect(readableMinutes(125)).toBe("2h 5m");
  });
});
