import { describe, expect, it } from "vitest";

/**
 * Every engine capability the client can call is reachable from the interface.
 *
 * # The bug this exists for
 *
 * Three times now a feature has shipped end to end — migration, repository, HTTP route, typed client
 * method, tests at every layer — and been reachable by nobody, because no component ever called it.
 * Audio retention was the clearest: the engine kept audio, swept it hourly, and served it over a
 * range-request endpoint, the transcript rendered a player for it, and the only way to switch it on
 * was `curl`. Before that, three of five connectors were absent from the catalogue the setup screen
 * read, so a screen that existed rendered nothing.
 *
 * None of those are type errors and none are lint errors. `api.ts` exports an object literal, so an
 * unused key is not an unused symbol — TypeScript has nothing to say about it. This test is the only
 * thing that can see the gap.
 *
 * # How it decides
 *
 * A method is reachable if some file outside `api.ts` mentions `api.thatMethod`. That is how every
 * caller in this app reaches the engine — there is no destructuring of the client anywhere, which
 * the last assertion here pins down, because destructuring would make this test blind.
 *
 * Whitespace between `api`, `.` and the name is allowed on purpose: the formatter breaks long chains
 * across lines, and a first draft of this test called nine reachable methods dead because it
 * required them to be adjacent.
 */

const API = "/src/lib/api.ts";
const SELF = "/src/lib/reachable.test.ts";

/**
 * Every `.ts`/`.tsx` file under `src`, as text, keyed by path.
 *
 * Vite's glob rather than `node:fs`, so this needs no `@types/node` — the only reason to add Node
 * types to a browser app would be this one test, and the bundler can already do it.
 */
const sources = import.meta.glob("/src/**/*.{ts,tsx}", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

/** The top-level keys of `export const api = { … }`. */
function apiMethods(text: string): string[] {
  const start = text.indexOf("export const api = {");
  expect(start, "api.ts still exports an object called `api`").toBeGreaterThan(-1);

  let depth = 0;
  let i = text.indexOf("{", start);
  const from = i + 1;
  for (; i < text.length; i++) {
    if (text[i] === "{") depth++;
    else if (text[i] === "}" && --depth === 0) break;
  }

  const body = text.slice(from, i);
  return [...body.matchAll(/^ {2}([a-zA-Z][a-zA-Z0-9_]*)\s*[(:]/gm)].map((m) => m[1]);
}

/**
 * Client methods with no caller in the interface, as of this being written.
 *
 * Not an exemption list — a list of known gaps, and the assertions below hold it to being exactly
 * that. Each of these is a capability the engine has, the client can call, and no screen offers. Any
 * one of them is either a screen worth building or a method worth deleting; leaving it here is
 * saying "not yet", in writing, where the next person can see it.
 *
 * Grouped by what is missing:
 *
 * - **People have no screen.** `people`, `participants`, `addParticipant`, `personMeetings` — the
 *   graph knows who attends what and nothing renders it.
 * - **Ask and agent runs.** `ask` is grounded Q&A with citations; `ChatView` uses `chat` and
 *   `askNote` instead. `agentRuns` is the history of what the agent did, which is exactly the thing
 *   an autonomous feature most needs to show.
 * - **Vault and import.** `mirrorMeeting`, `importAudio`, `mergeWorkspace`, `related`,
 *   `appendSegments` — reachable through other paths or not at all.
 * - **Odds and ends.** `createDecision`/`decisions` (the panel reads decisions off the summary and
 *   can delete one, but not add one), `deleteTicket`, `deleteActionItem`,
 *   `setMcpServerAutoStart`, `suggestCompletion`.
 */
const KNOWN_UNREACHABLE = [
  "addParticipant",
  "agentRuns",
  "appendSegments",
  "ask",
  "createDecision",
  "decisions",
  "deleteActionItem",
  "deleteTicket",
  "importAudio",
  "mergeWorkspace",
  "mirrorMeeting",
  "note",
  "participants",
  "people",
  "personMeetings",
  "related",
  "setMcpServerAutoStart",
  "suggestCompletion",
];

describe("the interface can reach the engine", () => {
  const files = Object.entries(sources);
  const api = sources[API];
  expect(api, `${API} is readable`).toBeTruthy();

  const methods = apiMethods(api);

  // Excluded from the haystack: this file names every method in `KNOWN_UNREACHABLE` as a string, and
  // a test that reads itself as evidence proves nothing. Three drift tests in this repository have
  // been written that way and passed for that reason.
  const elsewhere = files.filter(([path]) => path !== API && path !== SELF);
  const callers = elsewhere.map(([, text]) => text).join("\n");

  const reaches = (name: string) =>
    new RegExp(String.raw`\bapi\s*\.\s*${name}\b`).test(callers);

  it("finds the client's methods at all", () => {
    // If the shape of api.ts changes, every assertion below passes vacuously. This is the guard.
    expect(methods.length).toBeGreaterThan(100);
    expect(methods).toContain("audioRetention");
  });

  it("offers every capability somewhere, except the gaps written down", () => {
    const unreachable = methods.filter((name) => !reaches(name));
    const surprises = unreachable.filter((name) => !KNOWN_UNREACHABLE.includes(name));

    expect(
      surprises,
      "These engine capabilities have a typed client method and no caller, so nobody using the " +
        "app can reach them. Add a caller, or record it in KNOWN_UNREACHABLE with a reason.",
    ).toEqual([]);
  });

  it("keeps the list of gaps honest", () => {
    // The other half. Without this the list only grows: a gap gets closed, the entry stays, and it
    // stops describing anything. An entry that is now reachable has to be deleted from it.
    const stale = KNOWN_UNREACHABLE.filter((name) => reaches(name));
    expect(
      stale,
      "These are listed as unreachable but something calls them now. Delete them from " +
        "KNOWN_UNREACHABLE — the list is meant to shrink.",
    ).toEqual([]);

    const gone = KNOWN_UNREACHABLE.filter((name) => !methods.includes(name));
    expect(gone, "These are listed as unreachable but no longer exist on the client.").toEqual([]);
  });

  it("is not blinded by destructuring the client", () => {
    // `const { ask } = api` would hide a call from every regex above. Nothing does it today; this
    // fails if something starts, because the alternative is a test that quietly stops working.
    // Built rather than written literally: the pattern below would otherwise match this very line,
    // which is how the first version of this assertion failed against itself.
    const pattern = new RegExp(String.raw`}\s*=\s*` + "api" + String.raw`\b`);
    const destructured = elsewhere.filter(([, text]) => pattern.test(text)).map(([path]) => path);

    expect(destructured, "Destructuring `api` hides callers from this test.").toEqual([]);
  });
});
