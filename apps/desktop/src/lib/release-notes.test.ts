import { describe, expect, it } from "vitest";

// `?raw` gives the file as text. Typed by `vite/client`, which tsconfig already pulls in for
// `import.meta.glob` — so this needs no `@types/node` and no suppressions.
import changelog from "../../../../CHANGELOG.md?raw";
import helpView from "../views/HelpView.tsx?raw";
import packageJson from "../../package.json?raw";
import tauriConf from "../../src-tauri/tauri.conf.json?raw";

/**
 * The release notes say the same thing in both places, and both match the version being shipped.
 *
 * # Why this is a test
 *
 * There are two changelogs on purpose: `CHANGELOG.md` for someone reading the repository, and
 * Help → What's new for someone using the app, which is deliberately shorter. Two copies of a fact
 * is drift waiting to happen, and it had already happened — the in-app notes said "Unreleased — in
 * development" and listed five features on the day 0.1.0 was tagged, having gone unedited through
 * connectors, the assistant, MCP tools, memory, scheduled jobs, retention and templates.
 *
 * Nothing noticed, because release notes are the one thing in a codebase with no consumer that
 * fails when they are wrong. This is the consumer.
 *
 * # What it does not check
 *
 * Not the wording — that is a judgement, and a test that compared prose would only force the two to
 * become one, which defeats having a short version. It checks the version number, which is a fact,
 * and that the newest entry is not a placeholder.
 */

/** The version in the changelog's newest `## x.y.z` heading. */
function newestInChangelog(text: string): string {
  const match = /^## (\d+\.\d+\.\d+)/m.exec(text);
  expect(match, "CHANGELOG.md has a `## x.y.z` heading").toBeTruthy();
  return (match as RegExpExecArray)[1];
}

/**
 * Just the `releases` array from `HelpView`, as text.
 *
 * Scoped rather than searching the whole file, because the file also contains the help topics — one
 * of which is titled "Why system audio does not work yet". The first version of the last assertion
 * below passed against that, and went on passing after the limitation was deleted from the release
 * notes: an assertion satisfied by text it was not looking at.
 */
function appReleases(text: string): string {
  const start = text.indexOf("const releases:");
  expect(start, "HelpView still holds a `releases` array").toBeGreaterThan(-1);
  const end = text.indexOf("\n  ];", start);
  expect(end, "the array is terminated").toBeGreaterThan(start);
  return text.slice(start, end);
}

/** The `version:` of the first entry in `WhatsNew`'s releases array. */
function newestInApp(text: string): string {
  const match = /version: "([^"]+)"/.exec(appReleases(text));
  expect(match, "HelpView still lists releases with a `version:`").toBeTruthy();
  return (match as RegExpExecArray)[1];
}

describe("release notes", () => {
  const shipped = JSON.parse(packageJson).version as string;

  it("agree on the version, in both changelogs and both manifests", () => {
    expect(newestInChangelog(changelog)).toBe(shipped);
    expect(newestInApp(helpView)).toBe(shipped);
    // The version a user sees in the title bar and in `About` comes from here, not package.json.
    expect(JSON.parse(tauriConf).version).toBe(shipped);
  });

  it("do not ship a placeholder as the newest entry", () => {
    // "Unreleased" is the correct heading while unreleased and the wrong one in a tagged build.
    // This fires the moment the version is bumped without the notes being written, which is the
    // sequence that produced the stale entry this test exists for.
    expect(newestInApp(helpView)).toMatch(/^\d+\.\d+\.\d+$/);
    expect(changelog).not.toMatch(/^## Unreleased/m);
  });

  it("state what the build cannot do, not only what it can", () => {
    // The unsigned build cannot capture macOS system audio, and a release note that omits that
    // sends people to look for a setting that is not there. Both versions say so.
    for (const [name, text] of [
      ["CHANGELOG.md", changelog],
      // The release notes only, not the whole file — see `appReleases`.
      ["HelpView.tsx", appReleases(helpView)],
    ] as const) {
      expect(text.toLowerCase(), `${name} says what is missing`).toContain("system audio");
    }
  });
});
