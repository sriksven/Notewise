import { describe, expect, it } from "vitest";

import sidebar from "../components/Sidebar.tsx?raw";
import { parseRoute, routeToHash, type Route } from "./router";
import { DEFERRED, offered } from "./scope";

describe("deferred sections", () => {
  it("are tickets, people and automation", () => {
    expect([...DEFERRED].sort()).toEqual(["jobs", "people", "tickets"]);
    expect(offered("tickets")).toBe(false);
    expect(offered("people")).toBe(false);
    expect(offered("jobs")).toBe(false);
    expect(offered("home")).toBe(true);
    expect(offered("notes")).toBe(true);
  });

  it("keep working addresses, so offering one again is a one-word change", () => {
    // Held back, not removed: if a deferred route stopped parsing, bringing the section back would
    // mean rebuilding its routing as well as un-hiding it.
    for (const name of DEFERRED) {
      const route = { name } as Route;
      expect(parseRoute(routeToHash(route)).name).toBe(name);
    }
  });

  it("are filtered where the sidebar draws its entries", () => {
    // The sidebar still lists the three, so they come back by editing `DEFERRED` alone; what must
    // hold is that the rendering goes through `offered`.
    expect(sidebar).toMatch(/group\.items\.filter\(\(item\) => offered\(item\.route\.name\)\)/);
  });
});
