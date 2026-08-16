/**
 * The local engine, as seen from the extension.
 *
 * The extension is just another loopback API client and gets no special access — the same surface
 * the CLI and the desktop frontend use. Nothing here reaches the network: `127.0.0.1` is refused by
 * `Server::bind` if it is ever pointed anywhere else.
 *
 * # Finding it
 *
 * The engine does not sit on one known port. The desktop shell picks the first free port in a
 * small window, because a `notewise serve` may already hold the first one — and a Manifest V3
 * `host_permissions` list is static, so it cannot name a port chosen at runtime.
 *
 * So the extension enumerates the window. That means it will knock on ports belonging to other
 * software, which is why nothing is sent until a candidate has identified itself as Notewise:
 * speaker names must not be posted to whatever happens to be listening on 47823.
 */

/** Must match `Server::DEFAULT_PORT` and `Server::DISCOVERY_PORTS`, and the manifest. */
export const FIRST_PORT = 47821;
export const PORT_COUNT = 10;

export const PORTS = Array.from({ length: PORT_COUNT }, (_, i) => FIRST_PORT + i);

const origin = (port) => `http://127.0.0.1:${port}`;

/** How long a single probe may take. A port with nothing on it refuses immediately; one with
 *  unrelated software on it may not answer at all, and must not hold up the others. */
const PROBE_TIMEOUT_MS = 700;

async function withTimeout(promise, ms) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  try {
    return await promise(controller.signal);
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Whether this port is a Notewise engine, and whether it is recording.
 *
 * The shape check is the safety property. Something else on a loopback port may well answer
 * `/health` with a 200 and a JSON body; it will not answer with these fields. Anything that
 * fails the check is treated as not-Notewise and never receives a name.
 */
async function identify(port, fetchImpl) {
  try {
    const response = await withTimeout(
      (signal) =>
        fetchImpl(`${origin(port)}/health`, {
          method: "GET",
          headers: { accept: "application/json" },
          signal,
        }),
      PROBE_TIMEOUT_MS,
    );
    if (!response.ok) return null;

    const body = await response.json();
    const isNotewise =
      body &&
      body.status === "ok" &&
      typeof body.schema_version === "number" &&
      "recording_meeting_id" in body;

    if (!isNotewise) return null;
    return { port, recordingMeetingId: body.recording_meeting_id ?? null };
  } catch {
    // Nothing listening, something that is not Notewise, or a timeout. All the same answer.
    return null;
  }
}

/**
 * Find the engine that is actually recording.
 *
 * Probed in parallel — ten sequential timeouts would be seven seconds before a meeting page
 * learned there was nothing to talk to.
 *
 * An engine that is recording wins over one that is merely running: with both a CLI and the
 * desktop app open, the speaker events belong to whichever is capturing the audio, and picking
 * the lowest port would as often as not attach names to the wrong database.
 */
export async function findEngine(fetchImpl = fetch) {
  const found = (await Promise.all(PORTS.map((port) => identify(port, fetchImpl)))).filter(Boolean);
  if (found.length === 0) return null;

  return found.find((engine) => engine.recordingMeetingId !== null) ?? found[0];
}

/**
 * Is the desktop app running, and is it recording?
 *
 * Returns `{ meetingId, origin }`, or null. Speaker events are only useful against a meeting that
 * exists, so this is what decides whether to start tracking at all. The origin comes back with it
 * so a caller does not have to search again for every batch.
 */
export async function activeRecording(fetchImpl = fetch) {
  const engine = await findEngine(fetchImpl);
  if (!engine || !engine.recordingMeetingId) return null;

  return { meetingId: engine.recordingMeetingId, origin: origin(engine.port) };
}

/**
 * Post a batch of speaker events.
 *
 * Returns true when the engine accepted them. A rejection is logged once by the caller rather than
 * retried: the engine rejects a batch for structural reasons — an unknown participant, a negative
 * timestamp — and re-sending the identical body would fail identically.
 */
export async function postSpeakerEvents(meetingId, batch, engineOrigin, fetchImpl = fetch) {
  const response = await fetchImpl(
    `${engineOrigin}/v1/meetings/${encodeURIComponent(meetingId)}/speaker-events`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(batch),
    },
  );

  return response.ok;
}
