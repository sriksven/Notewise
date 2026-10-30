import type { Route } from "./router";

/**
 * Sections that are built and kept, but not offered in this release.
 *
 * Tickets, People and Automation (scheduled jobs) are held back so the first release is about
 * meetings, notes and Ask; they come back in a later one. Nothing is deleted — the views, routes,
 * client calls and engine endpoints all stay, and their tests keep running — so offering a section
 * again is removing its name from this set.
 *
 * Every way into a deferred section checks here: its sidebar entry, its page, and the links into it
 * from elsewhere (participants, related items, citations, "make a ticket"). A section that is merely
 * missing from the sidebar but still reachable from a button is half-hidden, and creates things the
 * user then has no screen for.
 */
export const DEFERRED: ReadonlySet<Route["name"]> = new Set<Route["name"]>(["tickets", "people", "jobs"]);

/** Whether this build offers a section: its sidebar entry, its page, and links into it. */
export function offered(section: Route["name"]): boolean {
  return !DEFERRED.has(section);
}
