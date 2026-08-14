/**
 * The content script. A loader, and nothing else.
 *
 * A Manifest V3 content script is not an ES module, so it cannot `import`. The two ways around that
 * are a bundler or a dynamic import; this is the dynamic import, which keeps every source file
 * readable as shipped and unit-testable under `node --test` with no build step to get out of date.
 *
 * `session.js` and everything it pulls in must therefore be listed in `web_accessible_resources`.
 */

(async () => {
  try {
    const { run } = await import(chrome.runtime.getURL("src/session.js"));
    run();
  } catch (error) {
    // A meeting page must not be broken by this extension failing to load. Speaker names are an
    // enhancement; the recording works without them.
    console.warn("[notewise] speaker tracking did not start:", error);
  }
})();
