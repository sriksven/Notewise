import { useEffect, useMemo, useState } from "react";
import { Search, X } from "lucide-react";

import { api, type Meeting } from "../lib/api";

interface Props {
  meetings: Meeting[];
  selectedId: string | null;
  /** The meeting the engine is capturing into, or null. The only source of the red dot. */
  recordingId: string | null;
  onSelect: (id: string) => void;
}

/** How long to wait after the last keystroke before asking the engine. */
const SEARCH_DEBOUNCE_MS = 200;

/** A hit from full-text search, reduced to what this list can act on. */
interface Hit {
  id: string;
  title: string;
  snippet: string;
}

function startOfDay(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/**
 * A day heading a person would use.
 *
 * "Today" and "Yesterday" rather than dates, because that is how someone looks for a meeting
 * they were in this morning. Older than that and the date is the more useful label.
 */
function dayLabel(iso: string): string {
  const date = new Date(iso);
  const days = Math.round((startOfDay(new Date()) - startOfDay(date)) / 86_400_000);

  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return date.toLocaleDateString([], { weekday: "long" });
  return date.toLocaleDateString([], {
    month: "long",
    day: "numeric",
    year: date.getFullYear() === new Date().getFullYear() ? undefined : "numeric",
  });
}

function timeLabel(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function Row({
  meeting,
  selected,
  recording,
  snippet,
  onSelect,
}: {
  meeting: Meeting;
  selected: boolean;
  /** Whether the engine is capturing into this meeting right now. */
  recording: boolean;
  snippet?: string;
  onSelect: () => void;
}) {
  // A meeting with no `ended_at` is open, which is not the same as being recorded — one
  // created for an import, or left dangling by a crash, has no microphone behind it. Only the
  // engine's own answer earns the red dot.
  const open = meeting.ended_at === null;

  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        aria-current={selected ? "true" : undefined}
        className={`w-full rounded-lg px-2.5 py-2 text-left transition ${
          selected ? "bg-accent/[0.06]" : "hover:bg-accent/[0.03]"
        }`}
      >
        <div className="flex items-center gap-1.5">
          {recording && (
            <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-record" aria-hidden />
          )}
          <span className="truncate text-[13px] font-medium text-ink">
            {meeting.title}
          </span>
        </div>

        <div className="mt-0.5 flex items-baseline gap-1.5 text-[11px] text-ink-faint">
          <span>
            {recording ? "Recording" : open ? "Still open" : timeLabel(meeting.started_at)}
          </span>
        </div>

        {snippet && (
          // The matched text, so a search result explains itself without being opened.
          <p className="mt-1 line-clamp-2 text-[11px] leading-snug text-ink-muted">
            {snippet}
          </p>
        )}
      </button>
    </li>
  );
}

/**
 * The permanent meeting library.
 *
 * Always on screen rather than behind a toggle. Everything in this app is about one meeting at
 * a time, so which meeting is being looked at is the app's primary state — hiding it made
 * switching a two-click operation and left the window with no sense of place.
 *
 * The search box asks the engine rather than filtering titles here: what a person remembers
 * about a meeting is usually something that was said in it, not what it was called.
 */
export function MeetingLibrary({ meetings, selectedId, recordingId, onSelect }: Props) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Hit[] | null>(null);

  const trimmed = query.trim();

  useEffect(() => {
    if (trimmed.length === 0) {
      setHits(null);
      return;
    }

    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .search(trimmed, 40)
        .then((results) => {
          if (cancelled) return;

          // Collapsed to one row per meeting, keeping the best-ranked line as the excerpt.
          // Five hits in the same hour-long meeting are one place to go, not five, and a list
          // that repeats the same title five times buries the other meetings that matched.
          const byMeeting = new Map<string, Hit>();
          for (const hit of results) {
            if (!hit.meeting_id || byMeeting.has(hit.meeting_id)) continue;
            byMeeting.set(hit.meeting_id, {
              id: hit.meeting_id,
              title: hit.title,
              snippet: hit.snippet,
            });
          }
          setHits([...byMeeting.values()]);
        })
        // A failed search shows nothing found rather than an error banner over the whole app;
        // the next keystroke retries anyway.
        .catch(() => !cancelled && setHits([]));
    }, SEARCH_DEBOUNCE_MS);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [trimmed]);

  /** Meetings grouped under day headings, newest first. */
  const groups = useMemo(() => {
    const byDay = new Map<string, Meeting[]>();
    for (const meeting of meetings) {
      const label = dayLabel(meeting.started_at);
      byDay.set(label, [...(byDay.get(label) ?? []), meeting]);
    }
    return [...byDay.entries()];
  }, [meetings]);

  /**
   * Search results, paired with the meeting they belong to.
   *
   * A hit can name a meeting older than the loaded page, so the hit's own title is the
   * fallback — a result that cannot be resolved locally is still worth showing.
   */
  const results = useMemo(() => {
    if (!hits) return null;
    return hits.map((hit) => ({
      hit,
      meeting:
        meetings.find((m) => m.id === hit.id) ??
        ({
          id: hit.id,
          project_id: null,
          title: hit.title,
          source: "combined",
          started_at: new Date().toISOString(),
          ended_at: new Date().toISOString(),
        } satisfies Meeting),
    }));
  }, [hits, meetings]);

  return (
    <aside className="chrome flex w-[268px] shrink-0 flex-col border-r border-hairline bg-rail">
      <div className="px-3 pb-2 pt-3">
        <div className="relative">
          <Search
            size={14}
            className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-ink-faint"
            aria-hidden
          />
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search what was said"
            aria-label="Search meetings"
            className="w-full rounded-lg border border-hairline bg-surface py-1.5 pl-8 pr-7
                       text-[13px] text-ink outline-none transition
                       placeholder:text-ink-faint focus:border-hairline"
          />
          {query && (
            <button
              type="button"
              onClick={() => setQuery("")}
              aria-label="Clear search"
              className="absolute right-1.5 top-1/2 flex h-5 w-5 -translate-y-1/2 items-center
                         justify-center rounded text-ink-faint transition hover:text-ink"
            >
              <X size={13} aria-hidden />
            </button>
          )}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-4">
        {results ? (
          results.length === 0 ? (
            <p className="px-2.5 py-2 text-[12px] text-ink-faint">
              Nothing matched “{trimmed}”.
            </p>
          ) : (
            <>
              <h2 className="px-2.5 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wide text-ink-faint">
                {results.length} result{results.length === 1 ? "" : "s"}
              </h2>
              <ul className="space-y-0.5">
                {results.map(({ hit, meeting }) => (
                  <Row
                    key={hit.id}
                    meeting={meeting}
                    selected={hit.id === selectedId}
                    recording={meeting.id === recordingId}
                    snippet={hit.snippet}
                    onSelect={() => onSelect(hit.id)}
                  />
                ))}
              </ul>
            </>
          )
        ) : meetings.length === 0 ? (
          <p className="px-2.5 py-2 text-[12px] leading-relaxed text-ink-faint">
            Nothing recorded yet. Press the red button to start.
          </p>
        ) : (
          groups.map(([label, group]) => (
            <section key={label}>
              <h2 className="px-2.5 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-wide text-ink-faint">
                {label}
              </h2>
              <ul className="space-y-0.5">
                {group.map((meeting) => (
                  <Row
                    key={meeting.id}
                    meeting={meeting}
                    selected={meeting.id === selectedId}
                    recording={meeting.id === recordingId}
                    onSelect={() => onSelect(meeting.id)}
                  />
                ))}
              </ul>
            </section>
          ))
        )}
      </div>
    </aside>
  );
}
