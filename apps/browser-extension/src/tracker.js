/**
 * Turning polled "who is speaking now" observations into speaker turns.
 *
 * Platform-independent on purpose: every adapter in `platforms.js` reduces a very different DOM to
 * the same `Seen[]` shape, and everything after that point is shared. This is where the two
 * genuinely tricky parts live — the clock, and the fact that a poll is not an event.
 *
 * # The clock
 *
 * The engine wants milliseconds since the recording started. This page knows `performance.now()`.
 * Those are unrelated origins, and the offset between them cannot be recovered after the fact — so
 * it is established **once**, when tracking starts, and every timestamp is derived from it.
 *
 * `recordingStartedAt` is supplied by the caller, which learned it from the engine. If tracking
 * starts late — the user opens the meeting tab after starting the recording — the caller passes the
 * elapsed offset and turns land in the right place anyway.
 *
 * # A poll is not an event
 *
 * There is no "speaker changed" callback to subscribe to, so this samples. Each sample says who
 * looks like they are speaking *right now*, and a turn is the span between the sample where someone
 * started looking that way and the sample where they stopped. Two consequences:
 *
 * - A turn's edges are only as precise as `INTERVAL_MS`. That is why the engine treats these
 *   boundaries as weaker than acoustic ones and prefers `NamedClusterDiarizer`.
 * - Every sample re-reports an ongoing speaker. The engine coalesces contiguous turns from the same
 *   participant, so emitting one span per sample is correct but wasteful; this closes a turn only
 *   when the speaker actually stops, and sends whole turns.
 */

/**
 * How often to sample the page.
 *
 * 250 ms is below the shortest conversational turn worth attributing and cheap enough to run for an
 * hour without being noticeable. Faster buys precision the platform's own dominant-speaker
 * reporting does not have, so it would be false precision.
 */
export const INTERVAL_MS = 250;

/**
 * A turn shorter than this is dropped rather than reported.
 *
 * Dominant-speaker indicators flicker: a cough, a keyboard knock, or the moment two people collide
 * produces a sub-half-second blip on someone who was not really taking a turn. Those blips are the
 * main source of wrong names, because a stray 250 ms turn can still win the overlap against a
 * segment nobody else was reported for.
 */
export const MIN_TURN_MS = 600;

/**
 * Accumulates speaker turns from repeated observations of a meeting page.
 */
export class SpeakerTracker {
  /**
   * @param {() => number} now monotonic clock, in ms, on any origin
   * @param {number} startOffsetMs where `now()` sits relative to recording start
   */
  constructor(now = () => performance.now(), startOffsetMs = 0) {
    this.now = now;
    this.origin = now() - startOffsetMs;
    /** @type {Map<string, string>} id -> display name */
    this.participants = new Map();
    /** @type {Map<string, number>} id -> when this open turn began, on the recording clock */
    this.open = new Map();
    /** @type {{participant: string, start_ms: number, end_ms: number}[]} */
    this.closed = [];
    /** @type {string | null} */
    this.localId = null;
    /** Samples where the page could not be read at all. */
    this.blindSamples = 0;
  }

  /** Milliseconds since recording start, never negative. */
  elapsed() {
    return Math.max(0, Math.round(this.now() - this.origin));
  }

  /**
   * Fold one observation in.
   *
   * `seen === null` means the adapter could not read the page. That closes every open turn — an
   * unreadable page is not evidence that the last speaker is still talking — and is counted, so the
   * caller can tell a broken selector from a quiet meeting.
   *
   * @param {import('./platforms.js').Seen[] | null} seen
   */
  observe(seen) {
    const at = this.elapsed();

    if (seen === null) {
      this.blindSamples += 1;
      this.#closeAll(at);
      return;
    }

    const speakingNow = new Set();

    for (const person of seen) {
      this.participants.set(person.id, person.displayName);
      if (person.isLocal) this.localId = person.id;
      if (person.speaking) speakingNow.add(person.id);
    }

    // Someone who stopped: close their turn.
    for (const id of [...this.open.keys()]) {
      if (!speakingNow.has(id)) this.#close(id, at);
    }

    // Someone who started: open one. Several at once is normal and kept — people talk over each
    // other, and dropping the quieter one here would discard the evidence that lets the engine
    // recognise an ambiguous segment as ambiguous.
    for (const id of speakingNow) {
      if (!this.open.has(id)) this.open.set(id, at);
    }
  }

  #close(id, at) {
    const start = this.open.get(id);
    this.open.delete(id);
    if (start === undefined) return;
    if (at - start < MIN_TURN_MS) return;
    this.closed.push({ participant: id, start_ms: start, end_ms: at });
  }

  #closeAll(at) {
    for (const id of [...this.open.keys()]) this.#close(id, at);
  }

  /**
   * Take everything complete enough to send, leaving open turns open.
   *
   * Returns `null` when there is nothing worth a request. Open turns are deliberately not flushed:
   * an in-progress turn has no end yet, and inventing one would report a speaker as having stopped
   * when they had not.
   */
  drain() {
    if (this.closed.length === 0) return null;

    const turns = this.closed;
    this.closed = [];

    return {
      participants: [...this.participants].map(([id, displayName]) => ({
        id,
        display_name: displayName,
      })),
      turns,
      local_participant_id: this.localId,
    };
  }

  /** Close open turns and take everything, for use when the meeting ends. */
  finish() {
    this.#closeAll(this.elapsed());
    return this.drain();
  }
}
