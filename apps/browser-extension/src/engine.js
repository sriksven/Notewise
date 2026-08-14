/**
 * The local engine, as seen from the extension.
 *
 * The extension is just another loopback API client and gets no special access — the same surface
 * the CLI and the desktop frontend use. Nothing here reaches the network: `127.0.0.1` is refused by
 * `Server::bind` if it is ever pointed anywhere else.
 */

export const ENGINE_ORIGIN = "http://127.0.0.1:47821";

/**
 * Is the desktop app running, and is it recording?
 *
 * Returns the active recording's meeting id, or null. Speaker events are only useful against a
 * meeting that exists, so this is what decides whether to start tracking at all.
 */
export async function activeRecording(fetchImpl = fetch) {
  try {
    const response = await fetchImpl(`${ENGINE_ORIGIN}/v1/recording`, {
      method: "GET",
      headers: { accept: "application/json" },
    });
    if (!response.ok) return null;

    const body = await response.json();
    return body.recording === true && body.meeting_id ? body.meeting_id : null;
  } catch {
    // The desktop app is not running. Not an error worth surfacing: the user may simply not have
    // opened it, and a meeting page should not produce console noise because of that.
    return null;
  }
}

/**
 * Post a batch of speaker events.
 *
 * Returns true when the engine accepted them. A rejection is logged once by the caller rather than
 * retried: the engine rejects a batch for structural reasons — an unknown participant, a negative
 * timestamp — and re-sending the identical body would fail identically.
 */
export async function postSpeakerEvents(meetingId, batch, fetchImpl = fetch) {
  const response = await fetchImpl(
    `${ENGINE_ORIGIN}/v1/meetings/${encodeURIComponent(meetingId)}/speaker-events`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(batch),
    },
  );

  return response.ok;
}
