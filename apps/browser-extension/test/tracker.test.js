/**
 * Tests for the platform-independent half of the extension.
 *
 * Run with `node --test apps/browser-extension/` — no browser, no bundler, no dependencies. The DOM
 * adapters in `platforms.js` are not covered here: they assert against markup owned by Google,
 * Zoom, and Microsoft, so a passing test would only prove the fixture matches itself and would go
 * stale silently. What is testable without lying is everything after the DOM, which is where the
 * subtle behaviour is.
 */

import test from "node:test";
import assert from "node:assert/strict";

import { SpeakerTracker, MIN_TURN_MS } from "../src/tracker.js";
import { activeRecording, postSpeakerEvents } from "../src/engine.js";

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
  const notRecording = async () => ({ ok: true, json: async () => ({ recording: false }) });
  assert.equal(await activeRecording(notRecording), null);

  const recording = async () => ({
    ok: true,
    json: async () => ({ recording: true, meeting_id: "abc" }),
  });
  assert.equal(await activeRecording(recording), "abc");

  // The desktop app is not running. A meeting page must not fill the console over that.
  const refused = async () => {
    throw new Error("ECONNREFUSED");
  };
  assert.equal(await activeRecording(refused), null);
});

test("a batch is posted to the loopback engine as JSON", async () => {
  let seen = null;
  const fake = async (url, init) => {
    seen = { url, init };
    return { ok: true };
  };

  const batch = { participants: [{ id: "p1", display_name: "Priya" }], turns: [] };
  assert.equal(await postSpeakerEvents("m1", batch, fake), true);

  assert.equal(seen.url, "http://127.0.0.1:47821/v1/meetings/m1/speaker-events");
  assert.equal(seen.init.method, "POST");
  assert.deepEqual(JSON.parse(seen.init.body), batch);
});
