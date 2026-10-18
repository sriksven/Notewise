/**
 * How the meeting page reaches the engine: by asking the extension's service worker to.
 *
 * # Why the page cannot simply fetch
 *
 * A content script's requests are the page's requests. Chrome attributes them to the page's origin
 * — `https://meet.google.com` — and applies that origin's rules, not the extension's: the
 * `host_permissions` in the manifest do not apply to them. Two rules then stop a fetch to
 * `http://127.0.0.1:47821` outright. Local Network Access refuses a public page's request to the
 * loopback address space ("Permission was denied for this request to access the `loopback` address
 * space"), and CORS would refuse the answer anyway, because the engine sends no CORS headers — on
 * purpose, since permissive CORS on an unauthenticated loopback API would let any page read the
 * user's meetings.
 *
 * So the extension did nothing at all on a real meeting page. Every probe failed, every failure
 * was swallowed as "the app is not running", and nothing reached the engine — while the unit tests,
 * which hand `engine.js` a fake `fetch`, stayed green.
 *
 * The service worker runs on the extension's own origin, where `host_permissions` do apply. It
 * makes the requests; the page side sends it a message and gets the answer back.
 *
 * # What the worker will do for a page
 *
 * Exactly the three things the page side needs, each with its arguments checked: say whether an
 * engine is recording, announce a join, and post speaker events. The last takes the engine's origin
 * from the caller — the worker may have been stopped and restarted since it found it — and so it
 * refuses any origin that is not one of the loopback ports in the manifest. Whatever the page side
 * sends, the worker posts to loopback or nowhere.
 */

/** Message types, shared by both halves so neither can drift from the other. */
export const ASK = Object.freeze({
  ACTIVE_RECORDING: "notewise:active-recording",
  JOIN: "notewise:join",
  SPEAKER_EVENTS: "notewise:speaker-events",
});

/** Platforms a join may be announced for: what `platforms.js` reports as `platform`. */
const PLATFORMS = new Set(["meet", "zoom", "teams"]);

/** Longer than any meeting code or Teams thread id, short enough to refuse something odd. */
const MAX_ID_LENGTH = 512;

const isId = (value) =>
  typeof value === "string" && value.length > 0 && value.length <= MAX_ID_LENGTH;

/**
 * The page-side half: the three engine calls `session.js` makes, carried by `send`.
 *
 * Each call settles to the same fallback the direct version produced when nobody was listening —
 * `null` or `false` — when the worker cannot be reached. That happens for an ordinary reason: an
 * extension that is reloaded or updated leaves its old content scripts running on open pages, and
 * every message they send throws "Extension context invalidated". That is the same situation as the
 * app not running, and a meeting page must not fill its console over it.
 *
 * @param {(message: object) => Promise<unknown>} send `chrome.runtime.sendMessage`, or a fake
 */
export function engineViaWorker(send) {
  const ask = async (message, fallback) => {
    try {
      const answer = await send(message);
      return answer ?? fallback;
    } catch {
      return fallback;
    }
  };

  return {
    /** @returns {Promise<{ meetingId: string, origin: string } | null>} */
    activeRecording: () => ask({ type: ASK.ACTIVE_RECORDING }, null),

    /** @returns {Promise<boolean>} */
    postJoinSignal: async (platform, meetingId) =>
      (await ask({ type: ASK.JOIN, platform, meetingId }, false)) === true,

    /** @returns {Promise<boolean>} */
    postSpeakerEvents: async (meetingId, batch, origin) =>
      (await ask({ type: ASK.SPEAKER_EVENTS, meetingId, batch, origin }, false)) === true,
  };
}

/**
 * The worker-side half: answer one message from a content script.
 *
 * Returns a promise of the answer, or of `null` for anything it will not do. Never rejects — a
 * failed request is reported the same way an absent engine is, which is how the direct version
 * behaved too.
 *
 * @param {{
 *   selfId: string,
 *   origins: string[],
 *   activeRecording: () => Promise<{ meetingId: string, origin: string } | null>,
 *   postJoinSignal: (platform: string, meetingId: string) => Promise<boolean>,
 *   postSpeakerEvents: (meetingId: string, batch: object, origin: string) => Promise<boolean>,
 * }} engine
 */
export function workerHandler(engine) {
  const allowed = new Set(engine.origins);

  return async (message, sender) => {
    // `onMessage` only hears this extension, but it hears all of it. Only a content script — a
    // sender with a tab — is a page asking, and nothing else in this extension has a reason to.
    if (!sender || sender.id !== engine.selfId || !sender.tab) return null;
    if (!message || typeof message !== "object") return null;

    try {
      switch (message.type) {
        case ASK.ACTIVE_RECORDING:
          return await engine.activeRecording();

        case ASK.JOIN:
          if (!PLATFORMS.has(message.platform) || !isId(message.meetingId)) return null;
          return await engine.postJoinSignal(message.platform, message.meetingId);

        case ASK.SPEAKER_EVENTS:
          // Loopback or nowhere: names are posted only to a port the manifest lists.
          if (!allowed.has(message.origin) || !isId(message.meetingId)) return null;
          if (!message.batch || typeof message.batch !== "object") return null;
          return await engine.postSpeakerEvents(message.meetingId, message.batch, message.origin);

        default:
          return null;
      }
    } catch {
      return null;
    }
  };
}
