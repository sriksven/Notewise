import { useCallback, useEffect, useState } from "react";
import { Check, Loader2, SquareCheckBig, TicketCheck } from "lucide-react";

import { api, ApiError, type ActionItem, type Meeting } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  meetings: Meeting[];
  onNavigate: (route: Route) => void;
}

/** An action item with the meeting it came out of, which is most of what makes it findable. */
interface Task extends ActionItem {
  meetingId: string;
  meetingTitle: string;
}

/** How many meetings back to look. Action items live on meetings; this bounds the fan-out. */
const DEPTH = 40;

function isOpen(task: Task): boolean {
  return task.status !== "done" && task.status !== "cancelled";
}

function overdue(task: Task, now = Date.now()): boolean {
  return task.due_at !== null && new Date(task.due_at).getTime() < now && isOpen(task);
}

/**
 * Every commitment, across every meeting.
 *
 * The reason this page exists: an action item is agreed in one meeting and chased in another,
 * and reaching it should not require remembering which meeting produced it. The per-meeting
 * list is the record of what that meeting decided; this is the list of what is still owed.
 *
 * Assembled client-side from the per-meeting endpoints rather than a dedicated one. That is a
 * real cost — up to forty requests on a large workspace — and worth naming: the alternative was
 * a cross-meeting query in `storage` plus an endpoint, and this page is the thing that proves
 * whether anyone wants the view before that gets built.
 */
export function TasksView({ meetings, onNavigate }: Props) {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showDone, setShowDone] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const lists = await Promise.all(
        meetings.slice(0, DEPTH).map(async (meeting) => {
          const items = await api.actionItems(meeting.id).catch(() => []);
          return items.map((item) => ({
            ...item,
            meetingId: meeting.id,
            meetingTitle: meeting.title,
          }));
        }),
      );
      setTasks(lists.flat());
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load tasks.");
    } finally {
      setLoading(false);
    }
  }, [meetings]);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (task: Task) => {
    setBusyId(task.id);
    const next = isOpen(task) ? "done" : "todo";
    try {
      const updated = await api.updateActionItem(task.id, { status: next });
      setTasks((current) =>
        current.map((t) => (t.id === task.id ? { ...t, ...updated } : t)),
      );
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not update that.");
    } finally {
      setBusyId(null);
    }
  };

  const promote = async (task: Task) => {
    setBusyId(task.id);
    try {
      await api.promoteActionItem(task.id);
      onNavigate({ name: "tickets" });
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not file a ticket.");
    } finally {
      setBusyId(null);
    }
  };

  const open = tasks.filter(isOpen);
  const done = tasks.filter((task) => !isOpen(task));
  const visible = showDone ? [...open, ...done] : open;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <SquareCheckBig size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">My Tasks</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          {loading ? "Loading…" : `${open.length} open`}
          {!loading && done.length > 0 && ` · ${done.length} done`}
        </span>
        {done.length > 0 && (
          <label className="flex cursor-pointer items-center gap-1.5 text-[12px] text-ink-muted">
            <input
              type="checkbox"
              checked={showDone}
              onChange={(event) => setShowDone(event.target.checked)}
              className="h-3.5 w-3.5 accent-[var(--accent)]"
            />
            Show completed
          </label>
        )}
      </header>

      {error && (
        <p role="alert" className="border-b border-warn-line bg-warn px-8 py-2 text-[12.5px] text-warn-text">
          {error}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
        {loading ? (
          <p className="flex items-center justify-center gap-2 py-16 text-[12.5px] text-ink-faint">
            <Loader2 size={14} className="animate-spin" aria-hidden />
            Reading {Math.min(meetings.length, DEPTH)} meeting
            {meetings.length === 1 ? "" : "s"}
          </p>
        ) : visible.length === 0 ? (
          <div className="mx-auto max-w-md py-16 text-center">
            <p className="text-[13.5px] font-medium text-ink">
              {tasks.length === 0 ? "No action items yet" : "Nothing open"}
            </p>
            <p className="mt-1 text-[12.5px] leading-relaxed text-ink-muted">
              {tasks.length === 0
                ? "Summarize a meeting and anything that sounded like a commitment lands here."
                : "Everything agreed so far has been ticked off."}
            </p>
          </div>
        ) : (
          <ul className="mx-auto max-w-3xl card divide-y divide-hairline overflow-hidden">
            {visible.map((task) => {
              const closed = !isOpen(task);
              return (
                <li key={task.id} className="group flex items-start gap-3 px-4 py-3">
                  <button
                    type="button"
                    onClick={() => void toggle(task)}
                    disabled={busyId === task.id}
                    aria-label={closed ? `Reopen ${task.text}` : `Complete ${task.text}`}
                    className={`mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded
                                border transition ${
                                  closed
                                    ? "border-transparent bg-accent text-accent-on"
                                    : "border-hairline hover:border-ink-muted"
                                }`}
                  >
                    {busyId === task.id ? (
                      <Loader2 size={10} className="animate-spin" aria-hidden />
                    ) : (
                      closed && <Check size={11} strokeWidth={3} aria-hidden />
                    )}
                  </button>

                  <div className="min-w-0 flex-1">
                    <p
                      className={`text-[13px] leading-snug ${
                        closed ? "text-ink-faint line-through" : "text-ink"
                      }`}
                    >
                      {task.text}
                    </p>
                    <p className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[11.5px] text-ink-faint">
                      {task.owner && <span>{task.owner}</span>}
                      {task.due_at && (
                        <span className={overdue(task) ? "text-warn-text" : undefined}>
                          due {new Date(task.due_at).toLocaleDateString([], {
                            day: "numeric",
                            month: "short",
                          })}
                        </span>
                      )}
                      <button
                        type="button"
                        onClick={() =>
                          onNavigate({
                            name: "meeting",
                            id: task.meetingId,
                            tab: "summary",
                          })
                        }
                        className="truncate underline-offset-2 transition hover:text-ink hover:underline"
                      >
                        {task.meetingTitle}
                      </button>
                    </p>
                  </div>

                  {!closed && (
                    <button
                      type="button"
                      onClick={() => void promote(task)}
                      disabled={busyId === task.id}
                      title="File this as a ticket"
                      className="flex shrink-0 items-center gap-1 rounded-full border border-hairline
                                 px-2 py-1 text-[11.5px] text-ink-muted opacity-0 transition
                                 hover:bg-overlay hover:text-ink group-hover:opacity-100
                                 focus-visible:opacity-100"
                    >
                      <TicketCheck size={11} aria-hidden />
                      Ticket
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
