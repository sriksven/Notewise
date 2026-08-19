import { api } from "./api";

/**
 * Showing the notifications the engine has queued.
 *
 * # Why the frontend does this
 *
 * `NotificationRepository` has had `pending_on` and `mark_delivered` since the comms layer landed
 * and nothing ever drained them, because the engine has no way to raise an OS notification and
 * `src-tauri` is deliberately outside the workspace. So the engine decides *that* something should
 * be shown and this drains the queue — the same split `connector_outbox` uses for delivery.
 *
 * The browser notification API rather than a Tauri plugin: it works in the webview and in a
 * browser, which means this path can actually be tested.
 *
 * # Delivered means shown
 *
 * `mark_delivered` is called after the notification is constructed, never before. Marking first
 * would turn a failure to display into a silently dropped notification, and the evidence would be
 * the absence of something nobody was expecting.
 */

/** How often to look for queued notifications. */
const POLL_MS = 15_000;

/** Ids shown in this session, so a slow round trip cannot double-display one. */
const shown = new Set<string>();

function titleFor(sourceKind: string): string {
  switch (sourceKind) {
    case "meeting":
      return "Meeting";
    case "action_item":
      return "Action item";
    case "decision":
      return "Decision";
    case "note":
      return "Note";
    case "ticket":
      return "Ticket";
    default:
      // Better a bare kind than a wrong friendly name: this list will fall behind the graph's
      // node kinds, and an unknown one should still produce a usable notification.
      return sourceKind.replace(/_/g, " ");
  }
}

/**
 * Show whatever is queued, once each.
 *
 * Returns how many were displayed, which is what makes this testable without inspecting the OS.
 */
export async function deliverPending(): Promise<number> {
  if (typeof Notification === "undefined") return 0;
  if (Notification.permission !== "granted") return 0;

  let delivered = 0;
  try {
    const pending = await api.pendingNotifications();
    for (const item of pending) {
      if (shown.has(item.id)) continue;
      shown.add(item.id);

      new Notification(titleFor(item.source_kind), { body: item.body, tag: item.id });
      delivered += 1;

      try {
        await api.markNotificationDelivered(item.id);
      } catch {
        // It was shown. Failing to record that is better than not showing it, and the id is in
        // `shown` so this session will not repeat it.
      }
    }
  } catch {
    // The engine being unreachable is not worth surfacing over a notification poll.
  }
  return delivered;
}

/**
 * Ask for permission and start polling. Returns a stop function.
 *
 * Permission is requested rather than assumed, and a refusal is final — nothing retries, because a
 * prompt on a timer is how an app gets its notifications permanently blocked.
 */
export function startNotificationDelivery(): () => void {
  let stopped = false;

  void (async () => {
    if (typeof Notification === "undefined") return;
    if (Notification.permission === "default") {
      try {
        await Notification.requestPermission();
      } catch {
        return;
      }
    }
    if (Notification.permission !== "granted") return;

    while (!stopped) {
      await deliverPending();
      await new Promise((r) => setTimeout(r, POLL_MS));
    }
  })();

  return () => {
    stopped = true;
  };
}
