/**
 * Telling the engine a meeting started.
 *
 * # Why this is separate from speaker tracking
 *
 * The speaker tracker runs only while the engine is already recording, which is a privacy property
 * and not an optimisation. Join detection cannot work that way — the whole point is to speak
 * *before* anything is recording, because the user has not pressed record yet and that is the
 * problem being solved.
 *
 * So the two are separate, and what they send is separate. Speaker tracking sends names and speaking
 * times, and only during a recording. This sends the platform and an opaque key, once, and reads no
 * DOM at all — the meeting is recognised from the URL. Nothing here can see the roster, the chat, or
 * the page.
 *
 * # Why it gives up
 *
 * If the desktop app is not running there is nobody to tell, and the sensible thing is to try again
 * shortly in case it is starting. But a meeting page left open all afternoon must not knock on ten
 * loopback ports forever, and a prompt to record a meeting that began twenty minutes ago is worse
 * than no prompt — so the attempts are bounded and then it stops for good.
 */

/** How often to retry while nobody is listening. */
export const RETRY_MS = 30_000;

/**
 * How many times to try before giving up on this meeting.
 *
 * Ten attempts at thirty seconds is five minutes. Past that the meeting is underway and an offer to
 * record it from the beginning is an offer that cannot be kept.
 */
export const MAX_ATTEMPTS = 10;

/**
 * Announces at most one join signal per meeting, and stops.
 *
 * The state is deliberately per-meeting rather than per-tab: a single-page app navigating from one
 * call to another is a new meeting and worth a new signal, while the same call re-rendering is not.
 */
export class JoinAnnouncer {
  /**
   * @param {(platform: string, meetingId: string) => Promise<boolean>} post
   * @param {{ maxAttempts?: number }} options
   */
  constructor(post, { maxAttempts = MAX_ATTEMPTS } = {}) {
    this.post = post;
    this.maxAttempts = maxAttempts;

    /** The meeting currently being announced, as `platform:id`. */
    this.key = null;
    this.attempts = 0;
    this.accepted = false;
  }

  /**
   * Consider the page's current state.
   *
   * @param {{ platform: string, meetingId: string } | null} meeting
   * @returns {Promise<"announced" | "retrying" | "settled" | "gave-up" | "idle">}
   */
  async tick(meeting) {
    if (!meeting) {
      // Left the call, or never in one. Forget the meeting so returning to it announces again —
      // the engine deduplicates, so a repeat costs nothing, and a user who rejoined after an hour
      // should not be silently skipped by this side.
      this.reset();
      return "idle";
    }

    const key = `${meeting.platform}:${meeting.meetingId}`;
    if (key !== this.key) {
      // A different meeting, including the first one seen.
      this.key = key;
      this.attempts = 0;
      this.accepted = false;
    }

    if (this.accepted) return "settled";
    if (this.attempts >= this.maxAttempts) return "gave-up";

    this.attempts += 1;

    // A failure is indistinguishable from the app not being open, which is the common case and not
    // worth reporting on a meeting page.
    const ok = await this.post(meeting.platform, meeting.meetingId).catch(() => false);
    if (ok) {
      this.accepted = true;
      return "announced";
    }

    return this.attempts >= this.maxAttempts ? "gave-up" : "retrying";
  }

  reset() {
    this.key = null;
    this.attempts = 0;
    this.accepted = false;
  }
}
