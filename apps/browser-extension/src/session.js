/**
 * The content script: watch a meeting page, report who spoke, send nothing else.
 *
 * # What this does and does not send
 *
 * Sends: participant display names, and time spans saying who was speaking. That is it.
 *
 * Does not send: audio, video, chat, screen contents, the meeting title, the URL, or any page text
 * beyond the names in the participant roster. The desktop app already has the audio; duplicating it
 * here would be the expensive way to learn something cheap.
 *
 * # It only runs while the engine is recording
 *
 * Tracking starts when the local engine reports an active recording and stops when it stops. A
 * meeting page with no recording in progress is left completely alone — nothing is read, nothing is
 * buffered, no timer runs. That is a privacy property, not an optimisation: the extension should be
 * inert unless the user has already chosen to record.
 */

import { platformFor } from "./platforms.js";
import { SpeakerTracker, INTERVAL_MS } from "./tracker.js";
import { activeRecording, postSpeakerEvents } from "./engine.js";

/*
 * Loaded by `content.js` via dynamic import rather than being the content script itself: a
 * Manifest V3 content script is not a module and cannot use static `import`. Keeping the logic in
 * real modules is what lets `tracker.js` be unit-tested under `node --test` with no browser.
 */

/** How often to ask the engine whether a recording is running. */
const ENGINE_POLL_MS = 3_000;

/** How often to flush accumulated turns. */
const FLUSH_MS = 5_000;

/**
 * Samples of an unreadable page before giving up on this platform adapter.
 *
 * A page can be legitimately unreadable for a while — the roster panel is closed, the meeting is
 * still joining. But an adapter whose selectors have been broken by a vendor redesign will never
 * recover, and quietly sampling nothing for an hour hides that. At this point tracking stops and
 * says so, leaving the transcript to acoustic clustering with anonymous labels.
 *
 * 120 samples at 250 ms is about thirty seconds.
 */
const BLIND_SAMPLE_LIMIT = 120;

/** @type {import('./platforms.js').Platform | null} */
let platform = null;

/** @type {{ tracker: SpeakerTracker, meetingId: string, timer: number, flush: number } | null} */
let session = null;

function stop(reason) {
  if (!session) return;

  clearInterval(session.timer);
  clearInterval(session.flush);

  const final = session.tracker.finish();
  const { meetingId, origin } = session;
  session = null;

  if (final) {
    // Best effort: the meeting is over either way, and a failed final flush costs some labels
    // rather than the transcript.
    postSpeakerEvents(meetingId, final, origin).catch(() => {});
  }

  console.info(`[notewise] stopped tracking speakers: ${reason}`);
}

function start(meetingId, origin) {
  const tracker = new SpeakerTracker();

  const timer = setInterval(() => {
    if (!session) return;

    // `participants()` returns null when the adapter cannot read the page at all, which the tracker
    // records rather than mistaking for an empty meeting.
    tracker.observe(platform.participants());

    if (tracker.blindSamples > BLIND_SAMPLE_LIMIT) {
      stop(
        `could not read the ${platform.name} participant list — its markup has probably changed. ` +
          `Speakers will fall back to anonymous labels rather than be guessed.`,
      );
    }
  }, INTERVAL_MS);

  const flush = setInterval(async () => {
    if (!session) return;

    const batch = session.tracker.drain();
    if (!batch) return;

    const accepted = await postSpeakerEvents(
      session.meetingId,
      batch,
      session.origin,
    ).catch(() => false);
    if (!accepted) {
      // Dropped, not retried: the engine rejects a batch for structural reasons, so an identical
      // resend fails identically. Losing a batch costs some labels; a retry loop costs the meeting.
      console.warn("[notewise] the engine rejected a batch of speaker events; dropping it");
    }
  }, FLUSH_MS);

  session = { tracker, meetingId, origin, timer, flush };
  console.info(`[notewise] tracking speakers on ${platform.name} for meeting ${meetingId}`);
}

async function poll() {
  // The engine is located on every poll rather than cached: the desktop app may be opened after
  // the meeting page, and it may come back on a different port than it had last time.
  const found = await activeRecording();
  const meetingId = found?.meetingId ?? null;

  if (meetingId && !session) {
    start(meetingId, found.origin);
  } else if (session && meetingId !== session.meetingId) {
    // Either recording stopped, or a different meeting started.
    stop(meetingId ? "the recording moved to another meeting" : "the recording stopped");
    if (meetingId) start(meetingId, found.origin);
  }
}

/**
 * Start watching this page, if it is a meeting page we understand.
 *
 * Called by `content.js`. Returns the platform adapter in use, or null when the page is not one of
 * ours — in which case nothing is scheduled and the extension stays completely inert.
 */
export function run() {
  platform = platformFor(new URL(location.href));
  if (!platform) return null;

  setInterval(poll, ENGINE_POLL_MS);
  poll();

  // A tab closed mid-meeting should still hand over what it has.
  window.addEventListener("pagehide", () => stop("the tab closed"), { once: true });

  return platform;
}
