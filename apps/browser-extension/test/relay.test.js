/**
 * Tests for how the meeting page reaches the engine.
 *
 * The bug these exist for: the content script fetched `http://127.0.0.1:<port>` itself. Chrome
 * treats a content script's request as the meeting page's, and a public page may not reach loopback
 * — so on a real Meet page every probe was refused, every refusal read as "the app is not running",
 * and nothing ever reached the engine. The tests stayed green throughout, because they handed
 * `engine.js` a fake `fetch` and never asked where it would run.
 *
 * So the first test here is about where code runs, not what it computes: nothing the content script
 * loads may make a request. The engine calls live in the service worker, which these tests drive
 * through `relay.js` with fakes for both `chrome.runtime` and the engine.
 */

import { readFile } from "node:fs/promises";
import test from "node:test";
import assert from "node:assert/strict";

import { ASK, engineViaWorker, workerHandler } from "../src/relay.js";
import { ORIGINS, PORTS } from "../src/engine.js";

const source = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");

/** Every module reachable from `entry` through relative static imports, as manifest paths. */
async function reachable(entry) {
  const seen = new Set();
  const pending = [entry];
  while (pending.length > 0) {
    const path = pending.pop();
    if (seen.has(path)) continue;
    seen.add(path);

    const text = await source(path);
    for (const [, spec] of text.matchAll(/^\s*import\s[^;]*?from\s+"(\.\/[^"]+)"/gm)) {
      const dir = path.slice(0, path.lastIndexOf("/") + 1);
      pending.push(dir + spec.slice(2));
    }
  }
  return [...seen];
}

test("nothing the content script loads makes a request", async () => {
  // content.js imports session.js dynamically; everything after that is static imports.
  const content = await source("src/content.js");
  assert.match(content, /getURL\("src\/session\.js"\)/, "content.js still loads session.js");

  const pageSide = await reachable("src/session.js");
  assert.ok(pageSide.includes("src/relay.js"), `the page side reaches the worker: ${pageSide}`);

  for (const path of pageSide) {
    assert.notEqual(
      path,
      "src/engine.js",
      "the meeting page imports the engine client, so its requests would be the page's — refused",
    );
    assert.doesNotMatch(
      await source(path),
      /\bfetch\s*\(/,
      `${path} calls fetch, and on a meeting page that request is the page's: Chrome refuses it`,
    );
  }
});

test("everything the content script loads is web-accessible", async () => {
  // A module missing from the list fails its dynamic import on the page, and content.js swallows
  // that so the meeting is not broken — which makes it another way to do nothing silently.
  const manifest = JSON.parse(await source("manifest.json"));
  const accessible = manifest.web_accessible_resources.flatMap((entry) => entry.resources);

  for (const path of await reachable("src/session.js")) {
    assert.ok(accessible.includes(path), `${path} is imported on the page but not web-accessible`);
  }
  assert.ok(!accessible.includes("src/engine.js"), "only the worker loads the engine client");
});

test("the engine is called from a module service worker", async () => {
  const manifest = JSON.parse(await source("manifest.json"));
  assert.deepEqual(manifest.background, { service_worker: "src/background.js", type: "module" });
  assert.deepEqual(manifest.permissions, [], "messaging and a worker need no permission");

  const worker = await reachable("src/background.js");
  assert.ok(worker.includes("src/engine.js"), "the worker is what reaches the engine");
});

test("the worker posts only to the ports the manifest lists", () => {
  assert.deepEqual(
    ORIGINS,
    PORTS.map((port) => `http://127.0.0.1:${port}`),
  );
});

// ---------------------------------------------------------------- the worker's half

const SELF = "notewise-extension-id";
const PAGE = { id: SELF, tab: { id: 7 } };

/** An engine double that records what it was asked to do. */
function fakeEngine(overrides = {}) {
  const calls = [];
  return {
    calls,
    engine: {
      selfId: SELF,
      origins: ORIGINS,
      activeRecording: async () => {
        calls.push(["activeRecording"]);
        return { meetingId: "m1", origin: ORIGINS[2] };
      },
      postJoinSignal: async (...args) => {
        calls.push(["postJoinSignal", ...args]);
        return true;
      },
      postSpeakerEvents: async (...args) => {
        calls.push(["postSpeakerEvents", ...args]);
        return true;
      },
      ...overrides,
    },
  };
}

test("a content script is answered", async () => {
  const { engine, calls } = fakeEngine();
  const handle = workerHandler(engine);

  assert.deepEqual(await handle({ type: ASK.ACTIVE_RECORDING }, PAGE), {
    meetingId: "m1",
    origin: "http://127.0.0.1:47823",
  });
  assert.equal(await handle({ type: ASK.JOIN, platform: "meet", meetingId: "abc-defg-hij" }, PAGE), true);
  assert.deepEqual(calls, [["activeRecording"], ["postJoinSignal", "meet", "abc-defg-hij"]]);
});

test("speaker names are never posted anywhere but loopback", async () => {
  const { engine, calls } = fakeEngine();
  const handle = workerHandler(engine);
  const batch = { participants: [{ id: "p1", display_name: "Priya" }], turns: [] };

  for (const origin of [
    "https://collector.example",
    "http://127.0.0.1:8080",
    "http://localhost:47821",
    "http://127.0.0.1:47821.example",
    undefined,
  ]) {
    assert.equal(
      await handle({ type: ASK.SPEAKER_EVENTS, meetingId: "m1", batch, origin }, PAGE),
      null,
      `posted to ${origin}`,
    );
  }
  assert.deepEqual(calls, [], "nothing was sent for any of them");

  assert.equal(
    await handle({ type: ASK.SPEAKER_EVENTS, meetingId: "m1", batch, origin: ORIGINS[4] }, PAGE),
    true,
  );
  assert.deepEqual(calls, [["postSpeakerEvents", "m1", batch, "http://127.0.0.1:47825"]]);
});

test("only this extension's content scripts are served", async () => {
  const { engine, calls } = fakeEngine();
  const handle = workerHandler(engine);
  const ask = { type: ASK.ACTIVE_RECORDING };

  assert.equal(await handle(ask, { id: "some-other-extension", tab: { id: 1 } }), null);
  assert.equal(await handle(ask, { id: SELF }), null, "not from a page");
  assert.equal(await handle(ask, undefined), null);
  assert.deepEqual(calls, []);
});

test("malformed asks are refused rather than forwarded", async () => {
  const { engine, calls } = fakeEngine();
  const handle = workerHandler(engine);

  for (const message of [
    null,
    "notewise:active-recording",
    { type: "notewise:something-else" },
    { type: ASK.JOIN, platform: "webex", meetingId: "x" },
    { type: ASK.JOIN, platform: "meet", meetingId: "" },
    { type: ASK.JOIN, platform: "meet", meetingId: "x".repeat(10_000) },
    { type: ASK.SPEAKER_EVENTS, meetingId: "m1", batch: null, origin: ORIGINS[0] },
  ]) {
    assert.equal(await handle(message, PAGE), null, JSON.stringify(message)?.slice(0, 80));
  }
  assert.deepEqual(calls, []);
});

test("an engine that fails is an absent engine, not a rejection", async () => {
  const { engine } = fakeEngine({
    activeRecording: async () => {
      throw new TypeError("Failed to fetch");
    },
  });
  assert.equal(await workerHandler(engine)({ type: ASK.ACTIVE_RECORDING }, PAGE), null);
});

// ---------------------------------------------------------------- the page's half

test("the page side carries each call to the worker and returns its answer", async () => {
  const sent = [];
  const engine = engineViaWorker(async (message) => {
    sent.push(message);
    if (message.type === ASK.ACTIVE_RECORDING) return { meetingId: "m1", origin: ORIGINS[0] };
    return true;
  });

  assert.deepEqual(await engine.activeRecording(), { meetingId: "m1", origin: ORIGINS[0] });
  assert.equal(await engine.postJoinSignal("zoom", "98765432101"), true);
  assert.equal(await engine.postSpeakerEvents("m1", { turns: [] }, ORIGINS[0]), true);
  assert.deepEqual(
    sent.map((m) => m.type),
    [ASK.ACTIVE_RECORDING, ASK.JOIN, ASK.SPEAKER_EVENTS],
  );
});

test("a reloaded extension reads as nobody listening", async () => {
  // What every old content script meets after the extension is reloaded or updated.
  const invalidated = () => {
    throw new Error("Extension context invalidated.");
  };
  const engine = engineViaWorker(invalidated);

  assert.equal(await engine.activeRecording(), null);
  assert.equal(await engine.postJoinSignal("meet", "abc-defg-hij"), false);
  assert.equal(await engine.postSpeakerEvents("m1", { turns: [] }, ORIGINS[0]), false);

  // And a worker that answered nothing at all, which is what an unhandled message gets.
  const silent = engineViaWorker(async () => undefined);
  assert.equal(await silent.activeRecording(), null);
  assert.equal(await silent.postJoinSignal("meet", "abc-defg-hij"), false);
});
