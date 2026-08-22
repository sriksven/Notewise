import { useCallback, useEffect, useState } from "react";
import { Mail, Mic, Users } from "lucide-react";

import { api, ApiError, type Meeting, type Person } from "../lib/api";
import { relativeTime } from "../lib/format";
import type { Route } from "../lib/router";

interface Props {
  /** Which person the address bar says is open, if any. */
  personId?: string;
  onNavigate: (route: Route) => void;
}

/**
 * Who has been in your meetings, and which meetings each of them was in.
 *
 * # Why this is a list and not a directory
 *
 * People arrive as a side effect of meetings — a calendar invitee, a named speaker, a participant
 * added by hand. Nothing here creates a contact record for its own sake, and there is no field this
 * screen collects that a meeting did not already supply. So it reads as "who was there", which is
 * what the graph actually knows, rather than a CRM that happens to be empty.
 *
 * # The voice print marker
 *
 * Shown because it is the one fact about a person that is neither obvious nor harmless: it means a
 * recording of their voice is stored, and that they can be recognised in future meetings. Off by
 * default and managed under Settings; this screen only says who has one, so the answer to "what
 * does it know about me" is somewhere a person can find it.
 */
export function PeopleView({ personId, onNavigate }: Props) {
  const [people, setPeople] = useState<Person[]>([]);
  const [meetings, setMeetings] = useState<Meeting[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setPeople(await api.people());
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load people.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Their meetings, whenever the address bar names someone. Keyed on the id rather than on a click,
  // so a link straight to `#/people/<id>` works and a reload stays put.
  useEffect(() => {
    if (!personId) {
      setMeetings(null);
      return;
    }
    let cancelled = false;
    setMeetings(null);
    void api
      .personMeetings(personId)
      .then((found) => !cancelled && setMeetings(found))
      .catch((e) => {
        if (cancelled) return;
        setError(e instanceof ApiError ? e.message : "Could not load their meetings.");
        setMeetings([]);
      });
    return () => {
      cancelled = true;
    };
  }, [personId]);

  const selected = people.find((person) => person.id === personId);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Users size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">People</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          {loading
            ? "Loading…"
            : people.length === 0
              ? "Nobody yet"
              : `${people.length} ${people.length === 1 ? "person" : "people"}`}
        </span>
      </header>

      {error && (
        <p role="alert" className="border-b border-hairline px-8 py-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}

      {!loading && people.length === 0 ? (
        <div className="px-8 py-6">
          <p className="max-w-xl text-[13px] leading-relaxed text-ink-muted">
            People appear here once meetings have them — from a calendar invitation, from naming a
            speaker in a transcript, or from adding someone to a meeting by hand.
          </p>
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-[minmax(220px,280px)_1fr] overflow-hidden">
          <ul className="min-h-0 overflow-y-auto border-r border-hairline py-2">
            {people.map((person) => (
              <li key={person.id}>
                <button
                  type="button"
                  onClick={() => onNavigate({ name: "people", id: person.id })}
                  className={`flex w-full items-start gap-2 px-4 py-2 text-left transition
                              hover:bg-overlay ${person.id === personId ? "bg-overlay" : ""}`}
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[12.5px] text-ink">
                      {person.display_name}
                    </span>
                    {person.email && (
                      <span className="block truncate text-[11px] text-ink-faint">
                        {person.email}
                      </span>
                    )}
                  </span>
                  {person.has_voice_print && (
                    <Mic
                      size={11}
                      className="mt-1 shrink-0 text-ink-faint"
                      aria-label="Has a stored voice print"
                    />
                  )}
                </button>
              </li>
            ))}
          </ul>

          <div className="min-h-0 overflow-y-auto px-8 py-6">
            {!selected ? (
              <p className="text-[13px] text-ink-faint">Pick someone to see their meetings.</p>
            ) : (
              <>
                <h2 className="text-[16px] font-semibold text-ink">{selected.display_name}</h2>
                <div className="mt-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-[12px] text-ink-muted">
                  {selected.email && (
                    <span className="flex items-center gap-1">
                      <Mail size={11} aria-hidden />
                      {selected.email}
                    </span>
                  )}
                  {selected.has_voice_print && (
                    <span className="flex items-center gap-1">
                      <Mic size={11} aria-hidden />
                      Voice print stored
                    </span>
                  )}
                </div>

                <h3 className="mb-2 mt-6 text-[11px] font-semibold uppercase tracking-wider text-ink-faint">
                  Meetings
                </h3>
                {meetings === null ? (
                  <p className="text-[12px] text-ink-faint">Loading…</p>
                ) : meetings.length === 0 ? (
                  // Possible, and worth saying plainly: a person can be created by naming a speaker
                  // and then have that meeting deleted.
                  <p className="text-[12px] text-ink-faint">No meetings recorded for them.</p>
                ) : (
                  <ul className="space-y-1">
                    {meetings.map((meeting) => (
                      <li key={meeting.id}>
                        <button
                          type="button"
                          onClick={() =>
                            onNavigate({ name: "meeting", id: meeting.id, tab: "transcript" })
                          }
                          className="w-full rounded-lg px-2 py-1.5 text-left transition hover:bg-overlay"
                        >
                          <span className="block truncate text-[12.5px] text-ink">
                            {meeting.title}
                          </span>
                          <span className="text-[11px] text-ink-faint">
                            {relativeTime(meeting.started_at)}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
