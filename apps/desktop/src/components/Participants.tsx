import { useCallback, useEffect, useState } from "react";
import { Mic, Plus } from "lucide-react";

import { api, ApiError, type Person } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  meetingId: string;
  onNavigate: (route: Route) => void;
}

/**
 * Who was in this meeting.
 *
 * # Adding by name, not by picking from a list
 *
 * `POST /v1/meetings/:id/participants` takes either a `person_id` or a `display_name`, and the name
 * path runs `find_or_create_by_name`. So one text field does both jobs: an existing person is
 * matched, a new one is created, and the caller does not have to know which case they are in. A
 * picker would have to be a picker *plus* a create form to cover the same ground.
 *
 * # Why the voice-print marker is here too
 *
 * The same reason as on the People screen: it is the one thing about a person that means a recording
 * of their voice is kept. Someone looking at who was in a meeting is exactly who should be able to
 * see that.
 */
export function Participants({ meetingId, onNavigate }: Props) {
  const [people, setPeople] = useState<Person[]>([]);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setPeople(await api.participants(meetingId));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load who was there.");
    } finally {
      setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
  }, [load]);

  async function add() {
    const name = draft.trim();
    if (!name) return;

    setDraft("");
    setAdding(false);
    try {
      await api.addParticipant(meetingId, { display_name: name });
      // Reloaded rather than appended: the engine may have matched an existing person, so the row
      // that comes back is not necessarily new and the list has its own order.
      await load();
    } catch (e) {
      setDraft(name);
      setAdding(true);
      setError(e instanceof ApiError ? e.message : "Could not add them.");
    }
  }

  if (loading && people.length === 0) {
    return <p className="text-[12px] leading-relaxed text-ink-faint">Loading…</p>;
  }

  return (
    <div>
      {error && (
        <p role="alert" className="mb-2 text-[11.5px] text-danger-text">
          {error}
        </p>
      )}

      {people.length === 0 ? (
        <p className="text-[12px] leading-relaxed text-ink-faint">
          Nobody recorded. A calendar invitation fills this in; so does naming a speaker.
        </p>
      ) : (
        <ul className="flex flex-wrap gap-1.5">
          {people.map((person) => (
            <li key={person.id}>
              <button
                type="button"
                onClick={() => onNavigate({ name: "people", id: person.id })}
                title={`Everything ${person.display_name} was in`}
                className="flex items-center gap-1 rounded-full border border-hairline bg-surface
                           px-2 py-0.5 text-[11.5px] text-ink-muted transition hover:bg-overlay
                           hover:text-ink"
              >
                {person.display_name}
                {person.has_voice_print && (
                  <Mic size={9} aria-label="Has a stored voice print" className="text-ink-faint" />
                )}
              </button>
            </li>
          ))}
        </ul>
      )}

      {adding ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void add();
          }}
          className="mt-2"
        >
          <input
            autoFocus
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => {
              if (!draft.trim()) setAdding(false);
            }}
            placeholder="Their name"
            aria-label="Add someone to this meeting"
            className="w-full rounded border border-hairline bg-surface px-2 py-1 text-[12.5px]
                       text-ink placeholder:text-ink-faint"
          />
        </form>
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="mt-2 flex items-center gap-1 text-[11.5px] text-ink-faint transition
                     hover:text-ink"
        >
          <Plus size={11} aria-hidden />
          Add someone
        </button>
      )}
    </div>
  );
}
