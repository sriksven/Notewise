import { describe, expect, it } from "vitest";

import { isStale, STALE_MS } from "./notifications";

describe("isStale", () => {
  const now = Date.parse("2026-08-20T12:00:00Z");
  const at = (msAgo: number) => new Date(now - msAgo).toISOString();

  it("shows something queued just now", () => {
    expect(isStale(at(0), now)).toBe(false);
    expect(isStale(at(30_000), now)).toBe(false);
  });

  /**
   * A notification saying a meeting is starting, delivered forty minutes later, is worse than none:
   * the user goes looking for a meeting that is half over.
   */
  it("drops something queued while the app was closed", () => {
    expect(isStale(at(40 * 60 * 1000), now)).toBe(true);
    expect(isStale(at(STALE_MS + 1000), now)).toBe(true);
  });

  it("keeps one right on the boundary", () => {
    expect(isStale(at(STALE_MS), now)).toBe(false);
    expect(isStale(at(STALE_MS - 1), now)).toBe(false);
  });

  /** Showing something with an unreadable date is the recoverable mistake. */
  it("does not treat an unparseable date as stale", () => {
    expect(isStale("not a date", now)).toBe(false);
    expect(isStale("", now)).toBe(false);
  });

  /** A clock that went backwards over an NTP correction must not drop everything. */
  it("does not drop something dated in the future", () => {
    expect(isStale(new Date(now + 60_000).toISOString(), now)).toBe(false);
  });
});
