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
  });

  // `#/meetings` reads as the library rather than home: it is the plural of a meeting page,
  // and a user who trims the id off a link means "show me all of them".
  it("reads a bare meetings path as the library", () => {
    expect(parseRoute("#/meetings")).toEqual({ name: "library" });
  });

  it("reads the flat destinations", () => {
    expect(parseRoute("#/record")).toEqual({ name: "record" });
    expect(parseRoute("#/library")).toEqual({ name: "library" });
    expect(parseRoute("#/tasks")).toEqual({ name: "tasks" });
    expect(parseRoute("#/tickets")).toEqual({ name: "tickets" });
    expect(parseRoute("#/trash")).toEqual({ name: "trash" });
    expect(parseRoute("#/agent")).toEqual({ name: "agent" });
    expect(parseRoute("#/about")).toEqual({ name: "about" });
  });

  it("reads a note id when one is present", () => {
    expect(parseRoute("#/notes")).toEqual({ name: "notes", id: undefined });
    expect(parseRoute("#/notes/note-42")).toEqual({ name: "notes", id: "note-42" });
  });

  it("reads a help section, ignoring one it does not have", () => {
    expect(parseRoute("#/help")).toEqual({ name: "help", section: undefined });
    expect(parseRoute("#/help/support")).toEqual({ name: "help", section: "support" });
    expect(parseRoute("#/help/nonsense")).toEqual({ name: "help", section: undefined });
  });

  it("reads a settings section", () => {
    expect(parseRoute("#/settings")).toEqual({ name: "settings", section: undefined });
    expect(parseRoute("#/settings/appearance")).toEqual({
      name: "settings",
      section: "appearance",
    });
  });

  it("ignores a query string", () => {
    expect(parseRoute("#/notes?from=search")).toEqual({ name: "notes", id: undefined });
  });
});

describe("routeToHash", () => {
  // The property that matters: a route survives being written to the address bar and read back.
  it("round-trips every route", () => {
    const routes: Route[] = [
      { name: "home" },
      { name: "record" },
      { name: "library" },
      { name: "meeting", id: "abc-123", tab: "transcript" },
      { name: "meeting", id: "abc-123", tab: "notes" },
      { name: "meeting", id: "abc-123", tab: "ask" },
      { name: "notes", id: undefined },
      { name: "notes", id: "note-42" },
      { name: "tasks" },
      { name: "tickets" },
      { name: "trash" },
      { name: "agent" },
      { name: "help", section: undefined },
      { name: "help", section: "whats-new" },
      { name: "about" },
      { name: "settings", section: "appearance" },
    ];

    for (const route of routes) {
      expect(parseRoute(routeToHash(route))).toEqual(route);
    }
  });
});
