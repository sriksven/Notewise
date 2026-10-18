/**
 * The service worker: the only part of the extension that talks to the engine.
 *
 * It exists for one reason, which `relay.js` explains in full: a content script's requests count
 * as the meeting page's, and a public page may not reach loopback. Requests made here carry the
 * extension's origin, which the manifest's loopback `host_permissions` cover.
 *
 * Nothing is kept between messages. A Manifest V3 worker is stopped whenever it is idle and started
 * again for the next message, so anything held in memory here would vanish mid-meeting; every
 * answer is computed from the message and the engine.
 */

import { activeRecording, ORIGINS, postJoinSignal, postSpeakerEvents } from "./engine.js";
import { workerHandler } from "./relay.js";

const handle = workerHandler({
  selfId: chrome.runtime.id,
  origins: ORIGINS,
  activeRecording: () => activeRecording(),
  postJoinSignal: (platform, meetingId) => postJoinSignal(platform, meetingId),
  postSpeakerEvents: (meetingId, batch, origin) => postSpeakerEvents(meetingId, batch, origin),
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  handle(message, sender).then(sendResponse);
  // The answer comes after a network round trip, so the channel has to stay open for it.
  return true;
});
