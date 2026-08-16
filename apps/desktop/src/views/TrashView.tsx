import { useCallback, useEffect, useState } from "react";
import { FileText, Loader2, RotateCcw, Trash2, Waves, X } from "lucide-react";

import { api, ApiError, type Meeting, type Note } from "../lib/api";
import { relativeTime } from "../lib/format";

/**
 * Deleted notes and meetings, and the way back.
 *
 * Tickets are absent on purpose: they mirror external trackers, where a delete has to
 * propagate rather than sit in a local limbo nothing else can see.
 *
 * Nothing here expires on a timer. A trash that empties itself after thirty days is a deadline
 * a user did not agree to, and this is their own machine — the only thing that destroys
 * anything is someone pressing the button that says so.
 */
export function TrashView() {
  const [notes, setNotes] = useState<Note[]>([]);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [emptying, setEmptying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmEmpty, setConfirmEmpty] = useState(false);

  const load = useCallback(async () => {
    try {
      const trash = await api.trash();
      setNotes(trash.notes);
      setMeetings(trash.meetings);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not read the trash.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const total = notes.length + meetings.length;

  /** Run one destructive or restorative action, keeping the row's spinner honest. */
  const act = async (id: string, run: () => Promise<unknown>, onDone: () => void) => {
    setBusyId(id);
    try {
      await run();
      onDone();
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "That did not work.");
    } finally {
      setBusyId(null);
    }
  };

  const empty = async () => {
    setEmptying(true);
    try {
      await api.emptyTrash();
      setNotes([]);
      setMeetings([]);
      setConfirmEmpty(false);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not empty the trash.");
    } finally {
      setEmptying(false);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Trash2 size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Trash</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          {loading ? "Loading…" : total === 0 ? "Empty" : `${total} item${total === 1 ? "" : "s"}`}
        </span>

        {total > 0 &&
          (confirmEmpty ? (
            <span className="flex items-center gap-2">
              <span className="text-[12px] text-ink-muted">
                {/* Named, because a meeting takes its transcript with it and a note does not. */}
                Delete all {total} for good
                {meetings.length > 0 &&
                  `, including ${meetings.length} transcript${meetings.length === 1 ? "" : "s"}`}
                ?
              </span>
              <button
                type="button"
                onClick={() => void empty()}
                disabled={emptying}
                className="rounded-full bg-danger-line px-2.5 py-1 text-[12px] font-medium
                           text-danger-text transition hover:opacity-90 disabled:opacity-50"
              >
                {emptying ? "Deleting…" : "Delete"}
              </button>
              <button
                type="button"
                onClick={() => setConfirmEmpty(false)}
                aria-label="Cancel"
                className="text-ink-faint transition hover:text-ink"
              >
                <X size={14} aria-hidden />
              </button>
            </span>
          ) : (
            <button
              type="button"
              onClick={() => setConfirmEmpty(true)}
              className="rounded-full border border-hairline px-2.5 py-1 text-[12px]
                         text-ink-muted transition hover:bg-overlay hover:text-ink"
            >
              Empty trash
            </button>
          ))}
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
            Loading
          </p>
        ) : total === 0 ? (
          <div className="mx-auto max-w-md py-16 text-center">
            <p className="text-[13.5px] font-medium text-ink">The trash is empty</p>
            <p className="mt-1 text-[12.5px] leading-relaxed text-ink-muted">
              Deleted notes and meetings wait here until you empty it. Nothing is removed on a
              timer.
            </p>
          </div>
        ) : (
          <div className="mx-auto max-w-3xl space-y-6">
            {meetings.length > 0 && (
              <Section title="Meetings" Icon={Waves}>
                {meetings.map((meeting) => (
                  <Row
                    key={meeting.id}
                    title={meeting.title}
                    detail={`Recorded ${relativeTime(meeting.started_at)} · deleted ${
                      meeting.deleted_at ? relativeTime(meeting.deleted_at) : "recently"
                    }`}
                    // Naming what goes with it. "Are you sure?" without saying what is
                    // destroyed is a reflex to click through.
                    warning="Its transcript, summaries, decisions and action items go too."
                    busy={busyId === meeting.id}
                    onRestore={() =>
                      void act(
                        meeting.id,
                        () => api.restoreMeeting(meeting.id),
                        () => setMeetings((m) => m.filter((x) => x.id !== meeting.id)),
                      )
                    }
                    onPurge={() =>
                      void act(
                        meeting.id,
                        () => api.purgeMeeting(meeting.id),
                        () => setMeetings((m) => m.filter((x) => x.id !== meeting.id)),
                      )
                    }
                  />
                ))}
              </Section>
            )}

            {notes.length > 0 && (
              <Section title="Notes" Icon={FileText}>
                {notes.map((note) => (
                  <Row
                    key={note.id}
                    title={note.title || "Untitled"}
                    detail={`Deleted ${note.deleted_at ? relativeTime(note.deleted_at) : "recently"}`}
                    preview={note.body.trim().slice(0, 200)}
                    busy={busyId === note.id}
                    onRestore={() =>
                      void act(
                        note.id,
                        () => api.restoreNote(note.id),
                        () => setNotes((n) => n.filter((x) => x.id !== note.id)),
                      )
                    }
                    onPurge={() =>
                      void act(
                        note.id,
                        () => api.purgeNote(note.id),
                        () => setNotes((n) => n.filter((x) => x.id !== note.id)),
                      )
                    }
                  />
                ))}
              </Section>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function Section({
  title,
  Icon,
  children,
}: {
  title: string;
  Icon: typeof Waves;
  children: React.ReactNode;
}) {
  return (
    <section>
      <h2 className="mb-1.5 flex items-center gap-1.5 px-1 text-[11px] font-semibold uppercase tracking-wider text-ink-faint">
        <Icon size={12} aria-hidden />
        {title}
      </h2>
      <ul className="card divide-y divide-hairline overflow-hidden">{children}</ul>
    </section>
  );
}

function Row({
  title,
  detail,
  preview,
  warning,
  busy,
  onRestore,
  onPurge,
}: {
  title: string;
  detail: string;
  preview?: string;
  warning?: string;
  busy: boolean;
  onRestore: () => void;
  onPurge: () => void;
}) {
  const [confirming, setConfirming] = useState(false);

  return (
    <li className="flex items-start gap-3 px-4 py-3">
      <div className="min-w-0 flex-1">
        <p className="truncate text-[13px] text-ink">{title}</p>
        <p className="mt-0.5 text-[11.5px] text-ink-faint">{detail}</p>
        {preview && (
          <p className="mt-1 line-clamp-2 text-[12px] leading-snug text-ink-muted">{preview}</p>
        )}
        {confirming && warning && (
          <p className="mt-1 text-[11.5px] leading-snug text-warn-text">{warning}</p>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-1">
        {confirming ? (
          <>
            <button
              type="button"
              onClick={onPurge}
              disabled={busy}
              className="rounded-full bg-danger-line px-2.5 py-1 text-[12px] font-medium
                         text-danger-text transition hover:opacity-90 disabled:opacity-50"
            >
              {busy ? "Deleting…" : "Delete for good"}
            </button>
            <button
              type="button"
              onClick={() => setConfirming(false)}
              aria-label="Cancel"
              className="p-1 text-ink-faint transition hover:text-ink"
            >
              <X size={13} aria-hidden />
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              onClick={onRestore}
              disabled={busy}
              className="flex items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                         text-[12px] text-ink-muted transition hover:bg-overlay hover:text-ink
                         disabled:opacity-50"
            >
              {busy ? (
                <Loader2 size={11} className="animate-spin" aria-hidden />
              ) : (
                <RotateCcw size={11} aria-hidden />
              )}
              Restore
            </button>
            <button
              type="button"
              onClick={() => setConfirming(true)}
              disabled={busy}
              aria-label={`Permanently delete ${title}`}
              className="rounded-full p-1.5 text-ink-faint transition
                         hover:bg-overlay hover:text-danger-text disabled:opacity-50"
            >
              <Trash2 size={13} aria-hidden />
            </button>
          </>
        )}
      </div>
    </li>
  );
}
