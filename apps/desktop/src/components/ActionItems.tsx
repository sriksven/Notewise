import { useCallback, useEffect, useState } from "react";
import { CalendarClock, Plus, TicketCheck, User } from "lucide-react";

import { api, ApiError, type ActionItem } from "../lib/api";

interface Props {
  meetingId: string;
  /**
   * Bumped by the parent after summarizing, so freshly extracted items appear without the
   * user reloading. A summary run is the only thing that adds items behind this component's
   * back.
   */
  refreshToken?: number;
}

const OPEN = new Set(["todo", "in_progress"]);

function isDone(item: ActionItem): boolean {
  return item.status !== undefined && !OPEN.has(item.status);
}

/**
 * The meeting's commitments, editable in place.
 *
 * Reads meeting-scoped rather than summary-scoped: an item typed by hand here, or one whose
 * summary was later regenerated, belongs in this list too. `summary.action_items` is the
 * narrower "what this summary proposed" view and would silently drop both.
 *
 * Every mutation is optimistic and rolls back on failure. The alternative — waiting for the
 * round trip before the checkbox moves — makes ticking off five items feel broken on a
 * machine that is also transcribing audio.
 */
export function ActionItems({ meetingId, refreshToken = 0 }: Props) {
  const [items, setItems] = useState<ActionItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const [promoting, setPromoting] = useState<string | null>(null);
  const [promoted, setPromoted] = useState<Set<string>>(new Set());

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setItems(await api.actionItems(meetingId));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load action items.");
    } finally {
      setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
  }, [load, refreshToken]);

  async function toggle(item: ActionItem) {
    const next = isDone(item) ? "todo" : "done";
    const before = items;

    setItems((current) =>
      current.map((i) => (i.id === item.id ? { ...i, status: next } : i)),
    );
    try {
      await api.updateActionItem(item.id, { status: next });
    } catch (e) {
      setItems(before);
      setError(e instanceof ApiError ? e.message : "Could not save that change.");
    }
  }

  async function add() {
    const text = draft.trim();
    if (!text) return;

    setDraft("");
    setAdding(false);
    try {
      const created = await api.createActionItem(meetingId, { text });
      setItems((current) => [...current, created]);
      setError(null);
    } catch (e) {
      // Put the text back rather than discarding it — retyping a sentence someone just
      // dictated is the most annoying possible failure here.
      setDraft(text);
      setAdding(true);
      setError(e instanceof ApiError ? e.message : "Could not add that item.");
    }
  }

  async function promote(item: ActionItem) {
    setPromoting(item.id);
    try {
      await api.promoteActionItem(item.id);
      setPromoted((current) => new Set(current).add(item.id));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not create a ticket.");
    } finally {
      setPromoting(null);
    }
  }

  if (loading && items.length === 0) {
    return <p className="text-[12px] leading-relaxed text-ink-faint">Loading…</p>;
  }

  return (
    <div>
      {items.length === 0 && !adding ? (
        <p className="text-[12px] leading-relaxed text-ink-faint">
          Nothing captured yet. Summarize the meeting, or add a commitment by hand.
        </p>
      ) : (
        <ul className="space-y-1.5">
          {items.map((item) => {
            const done = isDone(item);
            return (
              <li key={item.id} className="group flex items-start gap-2">
                <input
                  type="checkbox"
                  checked={done}
                  onChange={() => void toggle(item)}
                  aria-label={done ? `Reopen: ${item.text}` : `Complete: ${item.text}`}
                  className="mt-[3px] h-3.5 w-3.5 shrink-0 cursor-pointer rounded
                             border-hairline accent-neutral-800"
                />
                <span className="min-w-0 flex-1">
                  <span
                    className={`text-[12.5px] leading-snug ${
                      done ? "text-ink-faint line-through" : "text-ink"
                    }`}
                  >
                    {item.text}
                  </span>{" "}
                  {/* An unassigned item is shown as unassigned rather than left blank: a
                      blank owner reads as a rendering bug, and the whole point is that
                      nobody picked it up. */}
                  <span
                    className={`inline-flex items-center gap-0.5 whitespace-nowrap text-[11px] ${
                      item.owner ? "text-ink-muted" : "text-warn-text"
                    }`}
                  >
                    <User size={10} aria-hidden />
                    {item.owner ?? "unassigned"}
                  </span>
                  {item.due_at && (
                    <span className="ml-1.5 inline-flex items-center gap-0.5 whitespace-nowrap text-[11px] text-ink-muted">
                      <CalendarClock size={10} aria-hidden />
                      {new Date(item.due_at).toLocaleDateString([], {
                        month: "short",
                        day: "numeric",
                      })}
                    </span>
                  )}
                </span>

                {promoted.has(item.id) ? (
                  <span
                    title="A ticket already exists for this"
                    className="mt-[2px] shrink-0 text-[10px] font-medium text-ink-faint"
                  >
                    ticketed
                  </span>
                ) : (
                  <button
                    type="button"
                    onClick={() => void promote(item)}
                    disabled={promoting === item.id}
                    title="Track this as a ticket. The action item stays — it is the record that this meeting produced the work."
                    aria-label={`Make a ticket from: ${item.text}`}
                    className="mt-[1px] shrink-0 text-ink-faint opacity-0 transition
                               hover:text-ink group-hover:opacity-100
                               disabled:opacity-50"
                  >
                    <TicketCheck size={13} aria-hidden />
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {adding ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void add();
          }}
          className="mt-2"
        >
          <input
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => {
              if (!draft.trim()) setAdding(false);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setDraft("");
                setAdding(false);
              }
            }}
            placeholder="What needs doing?"
            aria-label="New action item"
            className="w-full rounded-md border border-hairline bg-surface px-2 py-1
                       text-[12.5px] text-ink outline-none
                       placeholder:text-ink-faint focus:border-hairline"
          />
        </form>
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="mt-2 flex items-center gap-1 text-[11.5px] text-ink-faint
                     transition hover:text-ink"
        >
          <Plus size={11} aria-hidden />
          Add item
        </button>
      )}

      {error && (
        <p role="status" className="mt-2 text-[11px] leading-snug text-danger-text">
          {error}
        </p>
      )}
    </div>
  );
}
