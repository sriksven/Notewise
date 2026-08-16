import { describe, expect, it } from "vitest";

import { parseRoute, routeToHash, type Route } from "./router";

describe("parseRoute", () => {
  it("reads an empty or missing hash as home", () => {
    for (const hash of ["", "#", "#/"]) {
      expect(parseRoute(hash)).toEqual({ name: "home" });
    }
  });

  it("reads a meeting with its tab", () => {
    expect(parseRoute("#/meetings/abc-123/summary")).toEqual({
      name: "meeting",
      id: "abc-123",
      tab: "summary",
    });
  });

  // A link written by hand, or one from an older version that had different tabs, must land
  // somewhere sensible rather than render nothing.
  it("falls back to the transcript for an unknown tab", () => {
    expect(parseRoute("#/meetings/abc-123/nonsense")).toEqual({
      name: "meeting",
      id: "abc-123",
      tab: "transcript",
    });
    expect(parseRoute("#/meetings/abc-123")).toEqual({
      name: "meeting",
      id: "abc-123",
      tab: "transcript",
    });
  });

  it("never returns a blank screen for junk", () => {
    expect(parseRoute("#/does-not-exist/at-all")).toEqual({ name: "home" });
    expect(parseRoute("#/meetings")).toEqual({ name: "home" });
  });

  it("reads the flat destinations", () => {
    expect(parseRoute("#/notes")).toEqual({ name: "notes" });
    expect(parseRoute("#/tickets")).toEqual({ name: "tickets" });
    expect(parseRoute("#/about")).toEqual({ name: "about" });
  });

  it("reads a settings section", () => {
    expect(parseRoute("#/settings")).toEqual({ name: "settings", section: undefined });
    expect(parseRoute("#/settings/appearance")).toEqual({
      name: "settings",
      section: "appearance",
    });
  });

  it("ignores a query string", () => {
    expect(parseRoute("#/notes?from=search")).toEqual({ name: "notes" });
  });
});

describe("routeToHash", () => {
  // The property that matters: a route survives being written to the address bar and read back.
  it("round-trips every route", () => {
    const routes: Route[] = [
      { name: "home" },
      { name: "meeting", id: "abc-123", tab: "transcript" },
      { name: "meeting", id: "abc-123", tab: "ask" },
      { name: "notes" },
      { name: "tickets" },
      { name: "about" },
      { name: "settings", section: "appearance" },
    ];

    for (const route of routes) {
      expect(parseRoute(routeToHash(route))).toEqual(route);
    }
  });
});
