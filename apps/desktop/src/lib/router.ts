import { useCallback, useEffect, useSyncExternalStore } from "react";

/**
 * The window's address.
 *
 * Hash-based, which is not a fashion choice: the desktop shell binds port 0, so the window's
 * origin changes on every launch. A path-based route would depend on the engine serving the
 * SPA fallback for every URL shape forever, and a bookmark would break the moment the port
 * moved. A hash is owned entirely by the document.
 *
 * Small enough to own rather than take a routing library for. What is needed is a location, a
 * way to change it, and the back button — all of which the browser already provides.
 */

export type Route =
  | { name: "home" }
  | { name: "record" }
  | { name: "library" }
  | { name: "meeting"; id: string; tab: MeetingTab }
  | { name: "notes"; id?: string }
  | { name: "tasks" }
  | { name: "tickets" }
  | { name: "people"; id?: string }
  | { name: "ask" }
  | { name: "trash" }
  | { name: "agent" }
  | { name: "jobs" }
  | { name: "connectors" }
  /**
   * The assistant panel, in its own window.
   *
   * A route rather than a component the shell reaches for directly: the overlay is a second
   * window pointed at the same frontend, and a hash is the only address that survives the engine
   * binding a different port on every launch.
   */
  | { name: "overlay" }
  | { name: "help"; section?: HelpSection }
  | { name: "settings"; section?: string }
  | { name: "about" };

export type MeetingTab = "transcript" | "summary" | "notes" | "ask";

const MEETING_TABS: MeetingTab[] = ["transcript", "summary", "notes", "ask"];

/** The pages under Help. Named so a link can point at one directly. */
export type HelpSection = "docs" | "support" | "whats-new" | "shortcuts";

const HELP_SECTIONS: HelpSection[] = ["docs", "support", "whats-new", "shortcuts"];

/** Parse a hash into a route. Anything unrecognised is home, never a blank screen. */
export function parseRoute(hash: string): Route {
  const path = hash.replace(/^#\/?/, "").split("?")[0];
  const parts = path.split("/").filter(Boolean);

  if (parts.length === 0) return { name: "home" };

  switch (parts[0]) {
    case "meetings": {
      // `#/meetings` with no id is the library, not a broken meeting page.
      if (!parts[1]) return { name: "library" };
      const tab = MEETING_TABS.includes(parts[2] as MeetingTab)
        ? (parts[2] as MeetingTab)
        : "transcript";
      return { name: "meeting", id: parts[1], tab };
    }
    case "record":
      return { name: "record" };
    case "library":
      return { name: "library" };
    case "notes":
      return { name: "notes", id: parts[1] };
    case "tasks":
      return { name: "tasks" };
    case "tickets":
      return { name: "tickets" };
    case "people":
      return { name: "people", id: parts[1] };
    case "ask":
      return { name: "ask" };
    case "trash":
      return { name: "trash" };
    case "agent":
      return { name: "agent" };
    case "jobs":
      return { name: "jobs" };
    case "connectors":
      return { name: "connectors" };
    case "overlay":
      return { name: "overlay" };
    case "help":
      return {
        name: "help",
        section: HELP_SECTIONS.includes(parts[1] as HelpSection)
          ? (parts[1] as HelpSection)
          : undefined,
      };
    case "settings":
      return { name: "settings", section: parts[1] };
    case "about":
      return { name: "about" };
    default:
      return { name: "home" };
  }
}

export function routeToHash(route: Route): string {
  switch (route.name) {
    case "home":
      return "#/";
    case "jobs":
      return "#/jobs";
    case "meeting":
      return `#/meetings/${route.id}/${route.tab}`;
    case "notes":
      return route.id ? `#/notes/${route.id}` : "#/notes";
    case "people":
      return route.id ? `#/people/${route.id}` : "#/people";
    case "help":
      return route.section ? `#/help/${route.section}` : "#/help";
    case "settings":
      return route.section ? `#/settings/${route.section}` : "#/settings";
    default:
      return `#/${route.name}`;
  }
}

function subscribe(onChange: () => void): () => void {
  window.addEventListener("hashchange", onChange);
  return () => window.removeEventListener("hashchange", onChange);
}

function snapshot(): string {
  return window.location.hash || "#/";
}

/**
 * The current route, and a way to change it.
 *
 * `navigate` assigns the hash, so the browser records history and the back button works without
 * this module keeping a stack of its own. `replace` is for corrections — landing on a meeting
 * that has since been deleted should not leave a broken entry to go back to.
 */
export function useRoute(): {
  route: Route;
  navigate: (to: Route) => void;
  replace: (to: Route) => void;
} {
  const hash = useSyncExternalStore(subscribe, snapshot, snapshot);

  const navigate = useCallback((to: Route) => {
    const next = routeToHash(to);
    if (window.location.hash !== next) window.location.hash = next;
  }, []);

  const replace = useCallback((to: Route) => {
    const next = routeToHash(to);
    const url = `${window.location.pathname}${window.location.search}${next}`;
    window.history.replaceState(null, "", url);
    // `replaceState` does not fire `hashchange`, so subscribers are told directly.
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  }, []);

  // A window opened with no hash at all should have one, so the first Back press does not
  // leave the app rather than moving within it.
  useEffect(() => {
    if (!window.location.hash) window.history.replaceState(null, "", "#/");
  }, []);

  return { route: parseRoute(hash), navigate, replace };
}
