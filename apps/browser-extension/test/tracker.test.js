/**
 * Tests for the platform-independent half of the extension.
 *
 * Run with `node --test apps/browser-extension/` — no browser, no bundler, no dependencies. The DOM
 * adapters in `platforms.js` are not covered here: they assert against markup owned by Google,
 * Zoom, and Microsoft, so a passing test would only prove the fixture matches itself and would go
 * stale silently. What is testable without lying is everything after the DOM, which is where the
 * subtle behaviour is.
 */

import { readFile } from "node:fs/promises";
import test from "node:test";
import assert from "node:assert/strict";

import { SpeakerTracker, MIN_TURN_MS } from "../src/tracker.js";
import { activeRecording, findEngine, postSpeakerEvents, PORTS } from "../src/engine.js";

/** A clock the test drives by hand. */
function fakeClock(start = 0) {
  let now = start;
  return {
    now: () => now,
    advance: (ms) => {
      now += ms;
    },
  };
}

function person(id, name, { speaking = false, isLocal = false } = {}) {
  return { id, displayName: name, speaking, isLocal };
}

test("a continuous speaker becomes one turn, not one per sample", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  // Twenty samples of Priya speaking.
  for (let i = 0; i < 20; i++) {
    tracker.observe([person("p1", "Priya", { speaking: true })]);
    clock.advance(250);
  }
  tracker.observe([person("p1", "Priya")]);

  const batch = tracker.drain();
  assert.equal(batch.turns.length, 1, "a poll must not emit one turn per sample");
  assert.equal(batch.turns[0].start_ms, 0);
  assert.equal(batch.turns[0].end_ms, 5_000);
});

test("turns are on the recording clock, not the page clock", () => {
  // A page whose performance.now() is already far along, tracking started 3s into the recording.
  const clock = fakeClock(900_000);
  const tracker = new SpeakerTracker(clock.now, 3_000);

  tracker.observe([person("p1", "Priya", { speaking: true })]);
  clock.advance(2_000);
  tracker.observe([person("p1", "Priya")]);

  const [turn] = tracker.drain().turns;
  assert.equal(turn.start_ms, 3_000, "the offset from recording start must be applied");
  assert.equal(turn.end_ms, 5_000);
});

test("a flicker shorter than the minimum turn is dropped", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  // A single sample of Marcus — a cough, or two people colliding.
  tracker.observe([person("p2", "Marcus", { speaking: true })]);
  clock.advance(250);
  tracker.observe([person("p2", "Marcus")]);

  assert.ok(250 < MIN_TURN_MS, "precondition: one sample is below the floor");
  assert.equal(tracker.drain(), null, "a blip must not become a turn that can win an overlap");
});

test("simultaneous speakers are both reported", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  tracker.observe([
    person("p1", "Priya", { speaking: true }),
    person("p2", "Marcus", { speaking: true }),
  ]);
  clock.advance(2_000);
  tracker.observe([person("p1", "Priya"), person("p2", "Marcus")]);

  const { turns } = tracker.drain();
  assert.equal(turns.length, 2, "cross-talk is evidence, not noise to be resolved here");
  assert.deepEqual(
    turns.map((t) => t.participant).sort(),
    ["p1", "p2"],
  );
});

test("an unreadable page closes open turns instead of extending them", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  tracker.observe([person("p1", "Priya", { speaking: true })]);
  clock.advance(2_000);

  // The adapter lost the roster.
  tracker.observe(null);
  clock.advance(60_000);
  tracker.observe(null);

  const { turns } = tracker.drain();
  assert.equal(turns.length, 1);
  assert.equal(turns[0].end_ms, 2_000, "a blind sample is not evidence that Priya kept talking");
  assert.equal(tracker.blindSamples, 2, "blind samples are counted so a broken selector is visible");
});

test("the local participant is reported so the engine can exclude them", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  tracker.observe([
    person("me", "Krishna", { speaking: true, isLocal: true }),
    person("p1", "Priya"),
  ]);
  clock.advance(2_000);
  tracker.observe([person("me", "Krishna", { isLocal: true }), person("p1", "Priya")]);

  assert.equal(tracker.drain().local_participant_id, "me");
});

test("a rename keeps one participant and takes the newer name", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  tracker.observe([person("p1", "iPhone", { speaking: true })]);
  clock.advance(2_000);
  tracker.observe([person("p1", "Priya")]);

  const { participants } = tracker.drain();
  assert.equal(participants.length, 1);
  assert.equal(participants[0].display_name, "Priya");
});

test("draining leaves an in-progress turn open rather than inventing an end", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  tracker.observe([person("p1", "Priya", { speaking: true })]);
  clock.advance(2_000);

  assert.equal(tracker.drain(), null, "an unfinished turn has no end to report");

  tracker.observe([person("p1", "Priya")]);
  assert.equal(tracker.drain().turns.length, 1, "and is sent once it finishes");
});

test("finish closes an open turn so the last speaker is not lost", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  tracker.observe([person("p1", "Priya", { speaking: true })]);
  clock.advance(3_000);

  const { turns } = tracker.finish();
  assert.equal(turns.length, 1);
  assert.equal(turns[0].end_ms, 3_000);
});

test("nothing is sent for a silent meeting", () => {
  const clock = fakeClock();
  const tracker = new SpeakerTracker(clock.now, 0);

  for (let i = 0; i < 10; i++) {
    tracker.observe([person("p1", "Priya"), person("p2", "Marcus")]);
    clock.advance(250);
  }

  assert.equal(tracker.drain(), null, "a roster with no speech labels nothing");
});

test("elapsed time never goes negative", () => {
  // A caller passing an offset larger than the clock has advanced must not produce a turn before
  // the recording started — the engine rejects those.
  const clock = fakeClock(0);
  const tracker = new SpeakerTracker(clock.now, 5_000);

  assert.ok(tracker.elapsed() >= 0);
});

test("no recording means no meeting id, and a dead engine is not an error", async () => {
  // Every port answers as a healthy engine that is idle.
  const idle = async () => ({
    ok: true,
    json: async () => ({ status: "ok", schema_version: 6, recording_meeting_id: null }),
  });
  assert.equal(await activeRecording(idle), null);

  const recording = async () => ({
    ok: true,
    json: async () => ({ status: "ok", schema_version: 6, recording_meeting_id: "abc" }),
  });
  assert.deepEqual(await activeRecording(recording), {
    meetingId: "abc",
    origin: "http://127.0.0.1:47821",
  });

  // The desktop app is not running. A meeting page must not fill the console over that.
  const refused = async () => {
    throw new Error("ECONNREFUSED");
  };
  assert.equal(await activeRecording(refused), null);
});

test("the engine is found wherever in the window it landed", async () => {
  // Only the third port is Notewise; the rest refuse, as they would with nothing listening.
  const onThirdPort = async (url) => {
    if (!url.startsWith("http://127.0.0.1:47823/")) throw new Error("ECONNREFUSED");
    return {
      ok: true,
      json: async () => ({ status: "ok", schema_version: 6, recording_meeting_id: "m9" }),
    };
  };

  assert.deepEqual(await activeRecording(onThirdPort), {
    meetingId: "m9",
    origin: "http://127.0.0.1:47823",
  });
});

/**
 * The safety property of probing. Something else on a loopback port may answer 200 with JSON;
 * it will not answer with the engine's fields, and a speaker's name must never be posted to it.
 */
test("software that is not Notewise is never sent anything", async () => {
  const somethingElse = async () => ({
    ok: true,
    json: async () => ({ status: "ok", message: "hello from an unrelated dev server" }),
  });

  assert.equal(await findEngine(somethingElse), null);
  assert.equal(await activeRecording(somethingElse), null);
});

test("a recording engine wins over an idle one", async () => {
  // The CLI is on the first port and idle; the desktop app is on the second and recording.
  const both = async (url) => {
    if (url.startsWith("http://127.0.0.1:47821/")) {
      return {
        ok: true,
        json: async () => ({ status: "ok", schema_version: 6, recording_meeting_id: null }),
      };
    }
    if (url.startsWith("http://127.0.0.1:47822/")) {
      return {
        ok: true,
        json: async () => ({ status: "ok", schema_version: 6, recording_meeting_id: "live" }),
      };
    }
    throw new Error("ECONNREFUSED");
  };

  const found = await activeRecording(both);
  assert.equal(found.meetingId, "live");
  assert.equal(found.origin, "http://127.0.0.1:47822", "names belong to whoever has the audio");
});

test("the probed window matches what the manifest asks permission for", async () => {
  const manifest = JSON.parse(
    await readFile(new URL("../manifest.json", import.meta.url), "utf8"),
  );
  const declared = manifest.host_permissions.map((p) => p.replace(/^http:\/\/127\.0\.0\.1:/, "").replace(/\/\*$/, ""));

  assert.deepEqual(
    declared,
    PORTS.map(String),
    "a port the extension probes but cannot reach is a silent failure to find the engine",
  );
});

test("a batch is posted to the loopback engine as JSON", async () => {
  let seen = null;
  const fake = async (url, init) => {
    seen = { url, init };
    return { ok: true };
  };

  const batch = { participants: [{ id: "p1", display_name: "Priya" }], turns: [] };
  assert.equal(
    await postSpeakerEvents("m1", batch, "http://127.0.0.1:47825", fake),
    true,
  );

  assert.equal(seen.url, "http://127.0.0.1:47825/v1/meetings/m1/speaker-events");
  assert.equal(seen.init.method, "POST");
  assert.deepEqual(JSON.parse(seen.init.body), batch);
});
