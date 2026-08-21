/**
 * Reading the roster and the active speaker out of a meeting page.
 *
 * # Why this is DOM scraping, and what follows from that
 *
 * Meet, Zoom, and Teams all know exactly who is talking — they route the audio — and all three
 * show it on screen. None of them offers a page-level API for it. So the only way to read it from
 * the user's own session is to read what is rendered.
 *
 * That is a maintenance treadmill: these vendors change their markup without notice, and every
 * selector below is a guess with a shelf life. The design constraint that follows is absolute:
 *
 *   **When a selector stops matching, emit nothing.**
 *
 * Never guess, never fall back to a neighbouring name, never keep reporting the last speaker seen.
 * A transcript with anonymous labels is a mild disappointment. A transcript that attributes words
 * to a named colleague who did not say them is a serious defect, and nothing downstream can tell
 * it from a correct one. Silence degrades cleanly to acoustic clustering; a wrong name does not
 * degrade at all.
 *
 * Each adapter therefore returns `null` from `participants()` when it cannot find the roster,
 * which the caller treats as "this page is not understood" rather than "nobody is here".
 *
 * # Why several selectors per field
 *
 * Each `SELECTORS` list is tried in order. The first entries are stable, semantic attributes
 * (`aria-*`, `data-*`, roles) which vendors change rarely because assistive technology depends on
 * them. Later entries are structural fallbacks. Generated class names are deliberately absent:
 * they change on every deploy and matching them produces an extension that breaks weekly.
 */

/**
 * @typedef {{ id: string, displayName: string, speaking: boolean, isLocal: boolean }} Seen
 */

/** Trim and collapse whitespace; return null for anything empty. */
function cleanName(raw) {
  if (typeof raw !== "string") return null;
  const name = raw.replace(/\s+/g, " ").trim();
  return name.length > 0 ? name : null;
}

/** First selector in `list` that matches anything under `root`. */
function firstMatch(root, list) {
  for (const selector of list) {
    const found = root.querySelectorAll(selector);
    if (found.length > 0) return found;
  }
  return null;
}

/**
 * A stable-enough id for a participant within one meeting.
 *
 * Prefers whatever the page already uses to key the tile, because that survives a rename. Falls
 * back to the display name, which does not — a rename then reads as a new participant, which is
 * wrong but harmless: two roster entries, both correctly named.
 */
function participantId(element, name) {
  const keyed =
    element.getAttribute("data-participant-id") ||
    element.getAttribute("data-tid") ||
    element.getAttribute("data-id") ||
    element.id;
  return keyed && keyed.trim().length > 0 ? keyed.trim() : `name:${name}`;
}

/**
 * Google Meet.
 *
 * The most tractable of the three: participant tiles carry `data-participant-id`, and Meet marks
 * the speaking tile with a dedicated attribute rather than only an animation.
 */
export const googleMeet = {
  name: "google-meet",
  platform: "meet",
  matches: (url) => /(^|\.)meet\.google\.com$/.test(url.hostname),

  /**
   * The meeting code, or null when this is not an actual call.
   *
   * `meet.google.com` on its own is a landing page, and a landing page is not a meeting. The code
   * is the three-four-three grouping Meet has used for years; `/lookup/<alias>` is the named-link
   * form that redirects to one.
   */
  meetingId(url) {
    const code = /^\/([a-z]{3}-[a-z]{4}-[a-z]{3})\/?$/.exec(url.pathname);
    if (code) return code[1];

    const lookup = /^\/lookup\/([A-Za-z0-9_-]{4,})\/?$/.exec(url.pathname);
    return lookup ? `lookup/${lookup[1]}` : null;
  },

  SELECTORS: {
    tile: ["[data-participant-id]", "[data-self-name]"],
    name: ["[data-self-name]", "[data-participant-name]", "[aria-label]"],
    // Meet toggles this attribute on the tile of whoever the SFU currently considers dominant.
    speaking: ["[data-is-speaking='true']", "[data-speaking='true']"],
  },

  participants() {
    const tiles = firstMatch(document, this.SELECTORS.tile);
    if (!tiles) return null;

    const seen = [];
    for (const tile of tiles) {
      const nameEl = firstMatch(tile, this.SELECTORS.name);
      const name =
        cleanName(nameEl?.[0]?.getAttribute("data-self-name")) ||
        cleanName(nameEl?.[0]?.getAttribute("data-participant-name")) ||
        cleanName(nameEl?.[0]?.getAttribute("aria-label")) ||
        cleanName(tile.getAttribute("aria-label")) ||
        cleanName(tile.textContent);

      // A tile whose name cannot be read is skipped rather than given a placeholder: an id with
      // no name produces a turn that the engine will reject anyway.
      if (!name) continue;

      seen.push({
        id: participantId(tile, name),
        displayName: name,
        speaking: this.SELECTORS.speaking.some((s) => tile.matches(s) || tile.querySelector(s)),
        // Meet marks the user's own tile with this attribute.
        isLocal: tile.hasAttribute("data-self-name"),
      });
    }

    return seen.length > 0 ? seen : null;
  },
};

/**
 * Zoom's web client.
 *
 * Zoom is hostile to automation, but this runs in the user's own authenticated session and only
 * reads — no synthetic input, no joining, nothing the user did not already do. The participants
 * panel must be open for the roster to exist in the DOM, which the caller surfaces rather than
 * working around.
 */
export const zoomWeb = {
  name: "zoom-web",
  platform: "zoom",
  matches: (url) => /(^|\.)zoom\.us$/.test(url.hostname),

  /**
   * The meeting number, or null.
   *
   * Zoom writes the same number four ways depending on how the link was made: `/j/` for an invite,
   * `/s/` for a host start link, and `/wc/<id>/join` or `/wc/join/<id>` for the web client. All of
   * them are the same meeting, so all of them produce the same id — otherwise one meeting joined by
   * two routes would look like two.
   */
  meetingId(url) {
    const patterns = [
      /^\/wc\/(\d{9,})\/(?:join|start)/,
      /^\/wc\/join\/(\d{9,})/,
      /^\/[js]\/(\d{9,})/,
    ];

    for (const pattern of patterns) {
      const found = pattern.exec(url.pathname);
      if (found) return found[1];
    }
    return null;
  },

  SELECTORS: {
    tile: [
      ".participants-item__item-view",
      "[class*='participants-li']",
      "[aria-label][class*='video-avatar']",
    ],
    name: [".participants-item__display-name", "[class*='display-name']"],
    // Zoom shows an active-audio indicator on the speaking participant's row.
    speaking: [
      "[class*='audio-volume-indicator']:not([class*='muted'])",
      "[class*='speaker-active']",
      "[aria-label*='is speaking']",
    ],
    local: ["[class*='participants-item__you']", "[aria-label*='(me)']"],
  },

  participants() {
    const tiles = firstMatch(document, this.SELECTORS.tile);
    if (!tiles) return null;

    const seen = [];
    for (const tile of tiles) {
      const nameEl = firstMatch(tile, this.SELECTORS.name);
      const name =
        cleanName(nameEl?.[0]?.textContent) ||
        cleanName(tile.getAttribute("aria-label")) ||
        cleanName(tile.textContent);
      if (!name) continue;

      seen.push({
        id: participantId(tile, name),
        displayName: name,
        speaking: this.SELECTORS.speaking.some((s) => tile.querySelector(s)),
        isLocal: this.SELECTORS.local.some((s) => tile.matches(s) || tile.querySelector(s)),
      });
    }

    return seen.length > 0 ? seen : null;
  },
};

/**
 * Microsoft Teams on the web.
 *
 * Teams uses `data-tid` attributes fairly consistently, which is the most durable hook of the
 * three. The proper route for Teams is a calling bot with unmixed audio, which needs an Azure app
 * and tenant admin consent — an enterprise integration, not something a user can switch on. This
 * gets names without any of that.
 */
export const teamsWeb = {
  name: "teams-web",
  platform: "teams",
  matches: (url) =>
    /(^|\.)teams\.microsoft\.com$/.test(url.hostname) ||
    /(^|\.)teams\.live\.com$/.test(url.hostname),

  /**
   * The meeting thread, or null.
   *
   * Teams is the awkward one: the join target lives in the *fragment*, which is why this reads
   * `url.hash` as well as the path. Everything else about Teams — chats, channels, files — is on
   * the same origin, so a host match alone would report a meeting every time somebody opened Teams
   * to read a message.
   */
  meetingId(url) {
    const wherever = `${url.pathname}${url.hash}`;

    // The thread id is the durable identity: the same meeting rejoined from a different link keeps
    // it, while the surrounding query parameters change every time.
    const thread = /19(?:%3a|:)meeting_([A-Za-z0-9%._-]+?)(?:%40|@)thread\.v2/i.exec(wherever);
    if (thread) return `19:meeting_${decodeURIComponent(thread[1])}@thread.v2`;

    // A join URL whose thread cannot be read is still a join URL. Reported without an id, which
    // the caller turns into a platform-and-time signal rather than nothing.
    return /meetup-join|\/v2\/\?meetingjoin=true|modern-calling/i.test(wherever)
      ? "meetup-join"
      : null;
  },

  SELECTORS: {
    tile: ["[data-tid='participant-item']", "[data-tid*='roster-list-item']", "[data-tid*='stream']"],
    name: ["[data-tid='participant-name']", "[data-tid*='display-name']", "[aria-label]"],
    speaking: ["[data-tid*='voice-level']", "[class*='speaking']", "[aria-label*='speaking']"],
    local: ["[data-tid*='you']", "[aria-label*='(You)']"],
  },

  participants() {
    const tiles = firstMatch(document, this.SELECTORS.tile);
    if (!tiles) return null;

    const seen = [];
    for (const tile of tiles) {
      const nameEl = firstMatch(tile, this.SELECTORS.name);
      const name =
        cleanName(nameEl?.[0]?.textContent) ||
        cleanName(nameEl?.[0]?.getAttribute("aria-label")) ||
        cleanName(tile.getAttribute("aria-label")) ||
        cleanName(tile.textContent);
      if (!name) continue;

      seen.push({
        id: participantId(tile, name),
        displayName: name,
        speaking: this.SELECTORS.speaking.some((s) => tile.querySelector(s)),
        isLocal: this.SELECTORS.local.some((s) => tile.matches(s) || tile.querySelector(s)),
      });
    }

    return seen.length > 0 ? seen : null;
  },
};

export const PLATFORMS = [googleMeet, zoomWeb, teamsWeb];

/** The adapter for a URL, or null when this is not a meeting page we understand. */
export function platformFor(url) {
  return PLATFORMS.find((p) => p.matches(url)) ?? null;
}

/**
 * Whether this page is an actual call, and which meeting.
 *
 * Returns `{ platform, meetingId }` or null. Separate from [`platformFor`] because the two answer
 * different questions: that one says "the extension understands this origin", and this one says
 * "somebody is in a meeting right now". The speaker tracker needs the first; join detection needs
 * the second, and conflating them would report a meeting every time a Teams tab was opened to read
 * a chat.
 *
 * URL-shaped rather than DOM-shaped on purpose. It is the one signal on these pages that does not
 * rot when a vendor redesigns, and navigating to a meeting link is a deliberate act — which is
 * exactly what the engine's rules treat as a strong signal.
 */
export function activeMeeting(url) {
  const platform = platformFor(url);
  if (!platform || typeof platform.meetingId !== "function") return null;

  const meetingId = platform.meetingId(url);
  return meetingId ? { platform: platform.platform, meetingId } : null;
}
