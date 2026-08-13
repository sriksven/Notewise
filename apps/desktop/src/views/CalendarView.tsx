import { CalendarDays } from "lucide-react";
import type { Meeting } from "../lib/api";

interface Props {
  meetings: Meeting[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

function dayKey(iso: string): string {
  return new Date(iso).toDateString();
}

function dayLabel(iso: string): string {
  const date = new Date(iso);
  const today = new Date();
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);

  if (date.toDateString() === today.toDateString()) return "Today";
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";

  return date.toLocaleDateString([], {
    weekday: "long",
    month: "long",
    day: "numeric",
    // Only show the year when it is not the current one — noise otherwise.
    year: date.getFullYear() === today.getFullYear() ? undefined : "numeric",
  });
}

function duration(meeting: Meeting): string {
  if (!meeting.ended_at) return "recording";
  const ms = new Date(meeting.ended_at).getTime() - new Date(meeting.started_at).getTime();
  const minutes = Math.round(ms / 60_000);
  return minutes < 1 ? "under a minute" : `${minutes} min`;
}

/**
 * Meetings grouped by day.
 *
 * A history view rather than a month grid: Notewise records what happened, and there is no
 * calendar integration yet, so there is nothing in the future to show. A grid of mostly-empty
 * squares would imply scheduling this does not do.
 */
export function CalendarView({ meetings, selectedId, onSelect }: Props) {
  if (meetings.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center px-6 text-center">
        <CalendarDays size={22} className="mb-2 text-neutral-300" aria-hidden />
        <p className="text-[13px] text-neutral-500">No meetings recorded yet.</p>
      </div>
    );
  }

  const days = new Map<string, Meeting[]>();
  for (const meeting of meetings) {
    const key = dayKey(meeting.started_at);
    days.set(key, [...(days.get(key) ?? []), meeting]);
  }

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
      <div className="mx-auto max-w-2xl space-y-6">
        <h1 className="text-[20px] font-semibold tracking-tight">History</h1>

        {[...days.entries()].map(([key, dayMeetings]) => (
          <section key={key}>
            <h2 className="mb-2 text-[11px] font-semibold uppercase tracking-wide text-neutral-400">
              {dayLabel(dayMeetings[0].started_at)}
            </h2>

            <ul className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
              {dayMeetings.map((meeting) => (
                <li key={meeting.id}>
                  <button
                    type="button"
                    onClick={() => onSelect(meeting.id)}
                    aria-current={meeting.id === selectedId ? "true" : undefined}
                    className={`flex w-full items-center gap-3 px-3 py-2.5 text-left transition ${
                      meeting.id === selectedId ? "bg-neutral-100" : "bg-white hover:bg-neutral-50"
                    }`}
                  >
                    <span className="w-14 shrink-0 font-mono text-[12px] tabular-nums text-neutral-400">
                      {new Date(meeting.started_at).toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                      })}
                    </span>

                    <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-neutral-800">
                      {meeting.title}
                    </span>

                    {!meeting.ended_at && (
                      <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-record" aria-hidden />
                    )}
                    <span className="shrink-0 text-[11px] text-neutral-400">
                      {duration(meeting)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}
