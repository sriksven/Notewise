import { describe, expect, it } from "vitest";

import { duration, relativeTime, size } from "./format";

describe("size", () => {
  it("switches unit at a gigabyte", () => {
    expect(size(77_000_000)).toBe("77 MB");
    expect(size(3_100_000_000)).toBe("3.1 GB");
  });
});

describe("relativeTime", () => {
  const now = new Date("2026-08-16T12:00:00Z");
  const ago = (ms: number) => new Date(now.getTime() - ms).toISOString();

  it("reads the last hour in minutes", () => {
    expect(relativeTime(ago(30_000), now)).toBe("just now");
    expect(relativeTime(ago(60_000), now)).toBe("1 minute ago");
    expect(relativeTime(ago(45 * 60_000), now)).toBe("45 minutes ago");
  });

  it("reads the last day in hours", () => {
    expect(relativeTime(ago(3 * 3_600_000), now)).toBe("3 hours ago");
    expect(relativeTime(ago(3_600_000), now)).toBe("1 hour ago");
  });

  it("names yesterday rather than counting hours", () => {
    expect(relativeTime(ago(30 * 3_600_000), now)).toBe("yesterday");
  });

  it("counts days up to a week", () => {
    expect(relativeTime(ago(3 * 86_400_000), now)).toBe("3 days ago");
  });

  // Past a week, "13 days ago" is arithmetic the reader has to do.
  it("falls back to a date past a week", () => {
    expect(relativeTime(ago(20 * 86_400_000), now)).toMatch(/Jul/);
  });

  it("includes the year only when it is not this one", () => {
    expect(relativeTime("2026-03-12T09:00:00Z", now)).not.toMatch(/2026/);
    expect(relativeTime("2024-03-12T09:00:00Z", now)).toMatch(/2024/);
  });

  // Clock skew is the only source of these, and "in 3 hours" for a meeting that already
  // happened is more alarming than useful.
  it("treats a future timestamp as just now", () => {
    expect(relativeTime(new Date(now.getTime() + 3_600_000).toISOString(), now)).toBe(
      "just now",
    );
  });

  it("does not throw on a malformed timestamp", () => {
    expect(relativeTime("not a date", now)).toBe("unknown");
  });
});

describe("duration", () => {
  it("pads seconds", () => {
    expect(duration(5_000)).toBe("0:05");
    expect(duration(65_000)).toBe("1:05");
  });

  it("grows an hours field only when needed", () => {
    expect(duration(59 * 60_000)).toBe("59:00");
    expect(duration(3_725_000)).toBe("1:02:05");
  });

  it("clamps a negative duration rather than printing a minus", () => {
    expect(duration(-1)).toBe("0:00");
  });
});
