import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, BellRing, Check, Chrome, CalendarClock, Radio } from "lucide-react";

import { api, type DetectionStatus } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  onNavigate: (route: Route) => void;
}

/**
 * Whether Notewise notices that a meeting started.
 *
 * # Why this screen is mostly diagnosis
 *
 * There is nothing to configure. Detection is on when a signal exists and off when none does, and
 * the only useful thing a settings screen can do is say which of the three reasons it is quiet:
 * the extension is not installed, no calendar is connected, or notifications were refused.
 *
 * Each of those is invisible otherwise. A user whose extension is not running sees a feature that
 * simply never fires, and there is no way to tell that from a feature that does not work.
 *
 * # Why the blind spot is on the screen
 *
 * The design excludes watching what is running on the machine, which means a meeting taken only in
 * the desktop Zoom or Teams app, with nothing on the calendar, is not detected at all. That is the
 * most likely reason somebody finds this underwhelming, and it belongs here rather than in a design
 * document nobody using the app will read.
 */
export function MeetingDetectionSettings({ onNavigate }: Props) {
  const [status, setStatus] = useState<DetectionStatus | null>(null);
  const [permission, setPermission] = useState<NotificationPermission | "unsupported">(
    typeof Notification === "undefined" ? "unsupported" : Notification.permission,
  );

  const load = useCallback(async () => {
    try {
      setStatus(await api.detectionStatus());
    } catch {
      // A status read that fails is not worth a banner over the whole settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const ask = async () => {
    if (typeof Notification === "undefined") return;
    setPermission(await Notification.requestPermission());
  };

  if (!status) return null;

  const lastSeen = (source: "extension" | "calendar") =>
    status.sources.find((entry) => entry.source === source)?.last_seen_at ?? null;

  const extensionHeard = lastSeen("extension");
  const calendarHeard = lastSeen("calendar");

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Radio size={13} className="text-ink-faint" aria-hidden />
        Noticing meetings
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        When a meeting starts, Notewise offers to record it — it never starts on its own. The offer
        appears at the top of this window and, if you allow notifications, outside it too.
      </p>

      <div className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
        <SignalRow
          icon={Chrome}
          title="Browser extension"
          ok={Boolean(extensionHeard)}
          detail={
            extensionHeard
              ? `Last reported ${relative(extensionHeard)}. Meetings opened in a browser tab are noticed.`
              : "Not heard from. Install the extension from apps/browser-extension and open a meeting in a tab — until then, browser meetings are not noticed."
          }
        />

        <SignalRow
          icon={CalendarClock}
          title="Calendar"
          ok={status.calendar_connected}
          detail={
            status.calendar_connected
              ? `Connected. An event with a meeting link waits ${Math.round(status.grace_secs / 60)} minute${
                  Math.round(status.grace_secs / 60) === 1 ? "" : "s"
                } before it counts — calendars are full of meetings nobody attends.${
                  calendarHeard ? ` Last matched ${relative(calendarHeard)}.` : ""
                }`
              : "No calendar connected, so nothing on your schedule is noticed."
          }
          action={
            status.calendar_connected
              ? undefined
              : { label: "Connect one", onClick: () => onNavigate({ name: "connectors" }) }
          }
        />

        <SignalRow
          icon={BellRing}
          title="Notifications"
          ok={permission === "granted"}
          detail={
            permission === "granted"
              ? "Allowed. You are told even when Notewise is behind another window."
              : permission === "denied"
                ? "Refused. Detection still works, but the offer only appears inside this window — you have to be looking at it. Allow notifications for Notewise in System Settings to change that."
                : permission === "unsupported"
                  ? "This build cannot raise notifications."
                  : "Not asked yet. Without this the offer only appears inside this window."
          }
          action={
            permission === "default"
              ? { label: "Allow notifications", onClick: () => void ask() }
              : undefined
          }
        />
      </div>

      <p className="mt-2 flex items-start gap-1.5 text-[11.5px] leading-relaxed text-ink-faint">
        <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
        {status.blind_spot}
      </p>
    </section>
  );
}

function SignalRow({
  icon: Icon,
  title,
  ok,
  detail,
  action,
}: {
  icon: typeof Radio;
  title: string;
  ok: boolean;
  detail: string;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="flex items-start gap-3 px-3 py-2.5">
      <Icon size={14} className="mt-0.5 shrink-0 text-ink-faint" aria-hidden />

      <div className="min-w-0 flex-1">
        <p className="flex items-center gap-1.5 text-[12.5px] text-ink">
          {title}
          {ok ? (
            <span className="flex items-center gap-0.5 rounded-full bg-ok px-1.5 py-0.5 text-[10.5px] text-ok-text">
              <Check size={9} strokeWidth={3} aria-hidden />
              working
            </span>
          ) : (
            <span className="rounded-full border border-hairline px-1.5 py-0.5 text-[10.5px] text-ink-faint">
              not active
            </span>
          )}
        </p>
        <p className="mt-0.5 text-[11.5px] leading-relaxed text-ink-faint">{detail}</p>
      </div>

      {action && (
        <button
          type="button"
          onClick={action.onClick}
          className="shrink-0 rounded-full border border-hairline px-2 py-0.5 text-[11.5px]
                     text-ink-muted transition hover:bg-overlay hover:text-ink"
        >
          {action.label}
        </button>
      )}
    </div>
  );
}

/** "4 minutes ago". Coarse on purpose: the question is whether it is talking, not exactly when. */
function relative(iso: string): string {
  const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 90) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)} minutes ago`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)} hours ago`;
  return new Date(iso).toLocaleDateString();
}
