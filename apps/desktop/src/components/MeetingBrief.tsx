import { useCallback, useEffect, useState } from "react";
import { History, Link2, Repeat } from "lucide-react";

import { api, ApiError, type Brief } from "../lib/api";

interface Props {
  meetingId: string;
  /** Jump to the previous instance of this series. */
  onOpenMeeting: (id: string) => void;
}

/**
 * What this meeting is still carrying from the last one.
 *
 * The thing a recurring meeting actually needs and no transcript can provide: the commitments
 * made three standups ago that nobody has closed. It is a graph traversal over stored state,
 * not a model call — so it costs nothing, works offline, and cannot invent an obligation
 * nobody agreed to.
 *
 * A meeting belongs to no series until someone says it does. That is deliberate: threading on
 * title alone would silently merge every meeting called "Sync", and the cost of a wrong merge
 * is a brief full of someone else's work.
 */
export function MeetingBrief({ meetingId, onOpenMeeting }: Props) {
  const [brief, setBrief] = useState<Brief | null>(null);
  const [loading, setLoading] = useState(true);
  const [linking, setLinking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setBrief(await api.brief(meetingId));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load the brief.");
    } finally {
      setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
  }, [load]);

  async function thread() {
    setLinking(true);
    try {
      // No arguments: the engine threads on this meeting's own title, which is what "this
      // one recurs" means before a calendar is connected.
      await api.assignSeries(meetingId);
      await load();
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not link this meeting.");
    } finally {
      setLinking(false);
    }
  }

  async function unthread() {
    setLinking(true);
    try {
      await api.assignSeries(meetingId, { clear: true });
      await load();
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not unlink this meeting.");
    } finally {
      setLinking(false);
    }
  }

  if (loading) {
    return <p className="text-[12px] leading-relaxed text-neutral-400">Loading…</p>;
  }

  if (!brief?.series) {
    return (
      <div>
        <p className="text-[12px] leading-relaxed text-neutral-400">
          Not part of a recurring meeting. Link it to carry unfinished work forward from
          earlier instances.
        </p>
        <button
          type="button"
          onClick={() => void thread()}
          disabled={linking}
          className="mt-2 flex items-center gap-1 text-[11.5px] text-neutral-400
                     transition hover:text-neutral-700 disabled:opacity-50"
        >
          <Link2 size={11} aria-hidden />
          {linking ? "Linking…" : "This meeting recurs"}
        </button>
        {error && <p className="mt-2 text-[11px] text-red-600">{error}</p>}
      </div>
    );
  }

  const carried = brief.unfinished_business;

  return (
    <div>
      <div className="mb-2 flex items-center gap-1.5">
        <Repeat size={11} className="shrink-0 text-neutral-400" aria-hidden />
        <span className="min-w-0 truncate text-[11.5px] text-neutral-500">
          {brief.series.title}
        </span>
        <button
          type="button"
          onClick={() => void unthread()}
          disabled={linking}
          title="Stop treating this as a recurring meeting"
          className="ml-auto shrink-0 text-[10.5px] text-neutral-300 transition
                     hover:text-neutral-600 disabled:opacity-50"
        >
          unlink
        </button>
      </div>

      {carried.length === 0 ? (
        <p className="text-[12px] leading-relaxed text-neutral-400">
          {brief.previous_meeting_id
            ? "Nothing outstanding from last time."
            : "First meeting in this series — nothing to carry forward yet."}
        </p>
      ) : (
        <ul className="space-y-1.5">
          {carried.map((item) => (
            <li key={item.id} className="flex items-start gap-2">
              <span className="mt-[6px] h-1.5 w-1.5 shrink-0 rounded-full bg-amber-400" />
              <span className="min-w-0">
                <span className="text-[12.5px] leading-snug text-neutral-800">
                  {item.text}
                </span>{" "}
                <span
                  className={`whitespace-nowrap text-[11px] ${
                    item.owner ? "text-neutral-500" : "text-amber-700"
                  }`}
                >
                  {item.owner ?? "unassigned"}
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}

      {brief.previous_meeting_id && (
        <button
          type="button"
          onClick={() => onOpenMeeting(brief.previous_meeting_id as string)}
          className="mt-2 flex items-center gap-1 text-[11.5px] text-neutral-400
                     transition hover:text-neutral-700"
        >
          <History size={11} aria-hidden />
          Open the previous one
        </button>
      )}

      {error && <p className="mt-2 text-[11px] text-red-600">{error}</p>}
    </div>
  );
}
