import { useCallback, useEffect, useState } from "react";
import { Loader2, RotateCcw, Trash2, X } from "lucide-react";

import { api, ApiError, type Note } from "../lib/api";
import { relativeTime } from "../lib/format";

/**
 * Deleted notes, and the way back.
 *
 * The trash only holds notes. Meetings own audio and transcripts and are not deletable from
 * the UI at all; tickets mirror external trackers, where a delete has to propagate rather than
 * sit in a local limbo. Adding either "for symmetry" would mean a `deleted_at` on every table
 * and a filter on every query that reads one.
 *
 * Nothing here expires on a timer. A trash that empties itself after thirty days is a deadline
 * a user did not agree to, and this is their own machine — the only thing that destroys a note
 * is someone pressing the button that says so.
 */
export function TrashView() {
  const [notes, setNotes] = useState<Note[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [emptying, setEmptying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmEmpty, setConfirmEmpty] = useState(false);

  const load = useCallback(async () => {
    try {
      setNotes(await api.trash());
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

  const restore = async (note: Note) => {
    setBusyId(note.id);
    try {
      await api.restoreNote(note.id);
      setNotes((current) => current.filter((n) => n.id !== note.id));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not restore that note.");
    } finally {
      setBusyId(null);
    }
  };

  const purge = async (note: Note) => {
    // Asks by name. A confirm dialog that says "are you sure?" without saying what it will
    // destroy is a reflex to click through.
    if (!window.confirm(`Permanently delete “${note.title || "Untitled"}”? This cannot be undone.`)) {
      return;
    }
    setBusyId(note.id);
    try {
      await api.purgeNote(note.id);
      setNotes((current) => current.filter((n) => n.id !== note.id));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not delete that note.");
    } finally {
      setBusyId(null);
    }
  };

  const empty = async () => {
    setEmptying(true);
    try {
      await api.emptyTrash();
      setNotes([]);
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
          {loading
            ? "Loading…"
            : notes.length === 0
              ? "Empty"
              : `${notes.length} note${notes.length === 1 ? "" : "s"}`}
        </span>

        {notes.length > 0 &&
          (confirmEmpty ? (
            <span className="flex items-center gap-2">
              <span className="text-[12px] text-ink-muted">
                Delete all {notes.length} for good?
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
        ) : notes.length === 0 ? (
          <div className="mx-auto max-w-md py-16 text-center">
            <p className="text-[13.5px] font-medium text-ink">The trash is empty</p>
            <p className="mt-1 text-[12.5px] leading-relaxed text-ink-muted">
              Deleted notes wait here until you empty it. Nothing is removed on a timer.
            </p>
          </div>
        ) : (
          <ul className="mx-auto max-w-3xl card divide-y divide-hairline overflow-hidden">
            {notes.map((note) => (
              <li key={note.id} className="flex items-start gap-3 px-4 py-3">
                <div className="min-w-0 flex-1">
                  <p className="truncate text-[13px] text-ink">{note.title || "Untitled"}</p>
                  <p className="mt-0.5 text-[11.5px] text-ink-faint">
                    Deleted {note.deleted_at ? relativeTime(note.deleted_at) : "recently"}
                  </p>
                  {note.body.trim() && (
                    <p className="mt-1 line-clamp-2 text-[12px] leading-snug text-ink-muted">
                      {note.body.trim().slice(0, 200)}
                    </p>
                  )}
                </div>

                <div className="flex shrink-0 items-center gap-1">
                  <button
                    type="button"
                    onClick={() => void restore(note)}
                    disabled={busyId === note.id}
                    className="flex items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                               text-[12px] text-ink-muted transition hover:bg-overlay hover:text-ink
                               disabled:opacity-50"
                  >
                    {busyId === note.id ? (
                      <Loader2 size={11} className="animate-spin" aria-hidden />
                    ) : (
                      <RotateCcw size={11} aria-hidden />
                    )}
                    Restore
                  </button>
                  <button
                    type="button"
                    onClick={() => void purge(note)}
                    disabled={busyId === note.id}
                    aria-label={`Permanently delete ${note.title || "Untitled"}`}
                    className="rounded-full p-1.5 text-ink-faint transition
                               hover:bg-overlay hover:text-danger-text disabled:opacity-50"
                  >
                    <Trash2 size={13} aria-hidden />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
