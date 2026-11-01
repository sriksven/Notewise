import { describe, expect, it } from "vitest";

import about from "../../../../core/crates/api-server/guide/about.md?raw";
import help from "../../../../core/crates/api-server/guide/help.md?raw";
import pages from "../../../../core/crates/api-server/guide/pages.md?raw";
import settings from "../../../../core/crates/api-server/guide/settings.md?raw";
import workspace from "../../../../core/crates/api-server/guide/workspace.md?raw";
import { offered } from "./scope";
import { parseRoute, routeToHash, SETTINGS_SECTIONS } from "./router";

/**
 * The engine's guide to this app points at pages of this app.
 *
 * Ask answers "how do I change models?" from a guide the engine ships, and each answer cites the page
 * it describes; the citation opens that page here. The guide lives in the engine and the pages live
 * here, so nothing but this test notices when one of them moves. A renamed Settings page would leave
 * every citation to it opening Home, and a new one would be a page Ask knows nothing about.
 *
 * What this cannot catch is a sentence in the guide that is no longer true. Nothing can, short of
 * reading it.
 */

const FILES = { about, help, pages, settings, workspace };

function guidePages(): Array<{ file: string; title: string; page: string }> {
  return Object.entries(FILES).flatMap(([file, text]) =>
    text
      .replace(/\r\n/g, "\n")
      .split("\n## ")
      .slice(1)
      .map((chunk) => {
        const [title, ...rest] = chunk.split("\n");
        const page = rest.find((line) => line.startsWith("page:"))?.slice("page:".length).trim();
        return { file, title, page: page ?? "" };
      }),
  );
}

describe("the engine's guide", () => {
  it("has sections", () => {
    expect(guidePages().length).toBeGreaterThan(10);
  });

  it("points every section at a page that opens as itself", () => {
    for (const { file, title, page } of guidePages()) {
      const route = parseRoute(`#/${page}`);
      // Home's own address is `#/`; anything else that reads as Home is a page that does not exist.
      const expected = page === "home" ? "#/" : `#/${page}`;
      expect(routeToHash(route), `${file}: "${title}" points at "${page}"`).toBe(expected);
      expect(offered(route.name), `${file}: "${title}" points at a held-back page`).toBe(true);
    }
  });

  it("describes every Settings page", () => {
    const described = new Set(guidePages().map(({ page }) => page));
    for (const id of SETTINGS_SECTIONS) {
      expect(described.has(`settings/${id}`), `nothing describes settings/${id}`).toBe(true);
    }
  });
});
