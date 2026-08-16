import { useMemo, useState } from "react";
import { Library, Mic, Search, Upload } from "lucide-react";

import { relativeTime } from "../lib/format";
import type { Meeting } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  meetings: Meeting[];
  recordingId: string | null;
  canRecord: boolean;
  onNavigate: (route: Route) => void;
  onImport: () => void;
}

/** Calendar buckets, coarsest first — the way someone looks for a meeting they half remember. */
function bucketOf(iso: string, now: Date): string {
  const then = new Date(iso);
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const elapsed = startOfToday - new Date(then.getFullYear(), then.getMonth(), then.getDate()).getTime();
  const days = Math.round(elapsed / 86_400_000);

  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  if (days < 30) return "This month";
  return then.toLocaleDateString([], {
    month: "long",
    ...(then.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

/**
 * Every meeting, grouped by when it happened.
 *
 * Grouped rather than a flat list with dates on each row: people locate a meeting by roughly
 * when it was ("the standup last week"), and a date column makes that a scan instead of a jump.
 *
 * Filtering here is on the title only, and deliberately so — it is a way to narrow a list you
 * are already looking at. Searching what was *said* is the top bar's job, because that needs
 * the index and returns a different kind of result.
 */
export function LibraryView({
  meetings,
  recordingId,
  canRecord,
  onNavigate,
  onImport,
}: Props) {
  const [filter, setFilter] = useState("");

  const groups = useMemo(() => {
    const now = new Date();
    const needle = filter.trim().toLowerCase();
    const matching = needle
      ? meetings.filter((m) => m.title.toLowerCase().includes(needle))
      : meetings;

    // Insertion order is preserved by Map, and `meetings` arrives newest first, so the
    // buckets come out in the right order without a second sort.
    const buckets = new Map<string, Meeting[]>();
    for (const meeting of matching) {
      const key = bucketOf(meeting.started_at, now);
      const existing = buckets.get(key);
      if (existing) existing.push(meeting);
      else buckets.set(key, [meeting]);
    }
    return [...buckets];
  }, [meetings, filter]);

  const total = meetings.length;
  const shown = groups.reduce((sum, [, list]) => sum + list.length, 0);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Library size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Library</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          {filter.trim() ? `${shown} of ${total}` : `${total} meeting${total === 1 ? "" : "s"}`}
        </span>

        <div className="relative">
          <Search
            size={13}
            className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-ink-faint"
            aria-hidden
          />
          <input
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            placeholder="Filter by title"
            aria-label="Filter meetings by title"
            className="w-52 rounded-full border border-hairline bg-surface py-1.5 pl-7 pr-3
                       text-[12.5px] text-ink outline-none transition
                       placeholder:text-ink-faint focus:border-accent"
          />
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
        {total === 0 ? (
          <div className="mx-auto max-w-md py-16 text-center">
            <p className="text-[13.5px] font-medium text-ink">No meetings yet</p>
            <p className="mx-auto mt-1 max-w-sm text-[12.5px] leading-relaxed text-ink-muted">
              Record one, or import audio you already have. Both are transcribed on this
              machine.
            </p>
            <div className="mt-5 flex justify-center gap-2">
              <button
                type="button"
                onClick={() => onNavigate({ name: "record" })}
                disabled={!canRecord}
                className="btn-accent"
              >
                <Mic size={14} aria-hidden />
                Record
              </button>
              <button type="button" onClick={onImport} className="btn-quiet">
                <Upload size={14} aria-hidden />
                Import audio
              </button>
            </div>
          </div>
        ) : shown === 0 ? (
          <p className="py-16 text-center text-[12.5px] text-ink-muted">
            No meeting title matches “{filter.trim()}”.
          </p>
        ) : (
          <div className="mx-auto max-w-3xl space-y-6">
            {groups.map(([label, list]) => (
              <section key={label}>
                <h2 className="mb-1.5 px-1 text-[11px] font-semibold uppercase tracking-wider text-ink-faint">
                  {label}
                </h2>
                <ul className="card divide-y divide-hairline overflow-hidden">
                  {list.map((meeting) => (
                    <li key={meeting.id}>
                      <button
                        type="button"
                        onClick={() =>
                          onNavigate({ name: "meeting", id: meeting.id, tab: "transcript" })
                        }
                        className="flex w-full items-center gap-3 px-4 py-2.5 text-left transition hover:bg-overlay"
                      >
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-[13px] text-ink">
                            {meeting.title}
                          </span>
                          <span className="block text-[11.5px] text-ink-faint">
                            {relativeTime(meeting.started_at)} · {meeting.source}
                          </span>
                        </span>
                        {meeting.id === recordingId && (
                          <span className="shrink-0 rounded-full bg-record/15 px-2 py-0.5 text-[10.5px] font-medium text-record">
                            live
                          </span>
                        )}
                      </button>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
