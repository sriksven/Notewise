import { describe, expect, it } from "vitest";

import tauriConf from "../../src-tauri/tauri.conf.json?raw";
import mainRs from "../../src-tauri/src/main.rs?raw";

/**
 * The packaged app looks for the frontend where the bundler puts it.
 *
 * # The bug this exists for
 *
 * `bundle.resources` was the list `["../dist"]`. Tauri sanitises `..` out of a bundled path, so the
 * frontend landed in `Notewise.app/Contents/Resources/_up_/dist` while `main.rs` resolved `"dist"`
 * against the resource directory and found nothing.
 *
 * That alone would have been a blank window. What made it worse is what happened next: the lookup
 * fell through to a development fallback built from `CARGO_MANIFEST_DIR`, which is baked in when the
 * binary is compiled. That directory exists on the machine that built the app and on no other, so
 * the `.dmg` aborted on launch — `Failed to setup app: no frontend found at
 * /Users/…/apps/desktop/dist` — quoting a path on the builder's disk. It worked perfectly for
 * whoever built it, which is why nothing caught it: not `cargo build`, not clippy, not CI, and not
 * `tauri build` itself, since the bundler was doing exactly what it was told.
 *
 * Only running the packaged app on a machine without the source tree reveals it. This test is the
 * cheap version of that.
 *
 * # What it checks
 *
 * That the two halves agree: whatever `main.rs` asks for by name is a path the bundle config
 * actually produces. It cannot prove the bundle works — for that the app has to be built and run
 * somewhere clean — but it fails on the specific mismatch that shipped.
 */
describe("the bundled frontend", () => {
  const config = JSON.parse(tauriConf);
  const resources = config.bundle.resources;

  /** Every path the frontend could be reached at inside the bundle. */
  const targets: string[] = Array.isArray(resources)
    ? // A list keeps the source path as the target, `..` and all.
      resources
    : Object.values(resources);

  /** What `frontend_dir` resolves against `BaseDirectory::Resource`. */
  const resolved = (() => {
    const match = /resolve\(\s*"([^"]+)"\s*,\s*tauri::path::BaseDirectory::Resource/.exec(mainRs);
    expect(match, "main.rs still resolves a resource path").toBeTruthy();
    return (match as RegExpExecArray)[1];
  })();

  it("is bundled to the path the app asks for", () => {
    expect(
      targets,
      `main.rs resolves "${resolved}" against the resource directory, and the bundle puts the ` +
        `frontend at ${JSON.stringify(targets)}. A packaged app cannot find its own interface.`,
    ).toContain(resolved);
  });

  it("declares resources as a map, so no target contains `..`", () => {
    // The array form is the trap: it is the obvious way to write it and it silently mangles the
    // path. A map states the target, which is the thing that has to match.
    expect(
      Array.isArray(resources),
      "Use `{ \"../dist\": \"dist\" }` rather than `[\"../dist\"]` — Tauri rewrites `..` to `_up_` " +
        "in the bundled path, and nothing in the config says so.",
    ).toBe(false);

    for (const target of targets) {
      expect(target, "a bundled target path cannot be relative").not.toContain("..");
    }
  });

  it("bundles what the frontend build actually writes", () => {
    // `frontendDist` is where Vite writes and where the dev server serves from. If the bundle
    // shipped a different directory, the packaged app would be a stale build rather than no build,
    // which is harder to notice.
    const sources = Array.isArray(resources) ? resources : Object.keys(resources);
    expect(sources).toContain(config.build.frontendDist);
  });
});
