import { useCallback, useEffect, useRef, useState } from "react";
import { ExternalLink, Loader2, Plus } from "lucide-react";

import { api, ApiError, type Note } from "../lib/api";
import { relativeTime } from "../lib/format";
import type { Route } from "../lib/router";

interface Props {
  meetingId: string | null;
  meetingTitle: string | null;
  /** Whether the meeting is being recorded right now, which changes what the empty state says. */
  isRecording: boolean;
  onNavigate: (route: Route) => void;
}

/** Debounce for the autosave, matching the standalone notes editor. */
const AUTOSAVE_MS = 800;

type SaveState = "idle" | "saving" | "saved" | "failed";

/**
 * Your own notes on a meeting.
 *
 * The thing the transcript cannot be: what *you* thought, as opposed to what was said. A
 * transcript is a record; this is the margin.
 *
 * Notes are real notes — they appear under Notes, survive the meeting, and can be asked
 * questions. The link is a graph edge rather than a column on the note, because a note is not
 * owned by the meeting it was taken in: it outlives it, and can reference more than one.
 *
 * Typing during a live meeting is the case this is designed around, which is why there is no
 * save button and no dialog between the cursor and the text: the first note is created by
 * clicking into an empty editor.
 */
export function MeetingNotes({ meetingId, meetingTitle, isRecording, onNavigate }: Props) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [body, setBody] = useState("");
  const [save, setSave] = useState<SaveState>("idle");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const persisted = useRef("");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** Guards against two rapid keystrokes both deciding to create the first note. */
  const creating = useRef(false);

  const selected = notes.find((note) => note.id === selectedId) ?? null;

  const load = useCallback(async () => {
    if (!meetingId) {
      setNotes([]);
      setSelectedId(null);
      setBody("");
      setLoading(false);
      return;
    }

    setLoading(true);
    try {
      const loaded = await api.meetingNotes(meetingId);
      setNotes(loaded);
      // Open the most recent, which is what `meetingNotes` returns first.
      const first = loaded[0] ?? null;
      setSelectedId(first?.id ?? null);
      setBody(first?.body ?? "");
      persisted.current = first?.body ?? "";
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load notes for this meeting.");
    } finally {
      setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
  }, [load]);

  const flush = useCallback(
    async (id: string, title: string, next: string) => {
      if (next === persisted.current) return;

      setSave("saving");
      try {
        const saved = await api.updateNote(id, title, next);
        persisted.current = saved.body;
        setNotes((current) => current.map((n) => (n.id === id ? saved : n)));
        setSave("saved");
        setError(null);
      } catch (e) {
        setSave("failed");
        setError(e instanceof ApiError ? e.message : "Could not save.");
      }
    },
    [],
  );

  /**
   * Create the first note for this meeting, titled after it.
   *
   * Called on the first keystroke rather than from a button: during a live meeting the gap
   * between deciding to write something and being able to type it should be zero.
   */
  const createFirst = useCallback(
    async (initial: string) => {
      if (!meetingId || creating.current) return;
      creating.current = true;
      try {
        const note = await api.createMeetingNote(meetingId, {
          title: meetingTitle ? `Notes — ${meetingTitle}` : "Meeting notes",
          body: initial,
        });
        setNotes((current) => [note, ...current]);
        setSelectedId(note.id);
        persisted.current = note.body;
        setSave("saved");
        setError(null);
      } catch (e) {
        setError(e instanceof ApiError ? e.message : "Could not create a note.");
      } finally {
        creating.current = false;
      }
    },
    [meetingId, meetingTitle],
  );

  // Debounced autosave, or a create when there is nothing to save into yet.
  useEffect(() => {
    if (!meetingId) return;
    if (timer.current) clearTimeout(timer.current);

    timer.current = setTimeout(() => {
      if (selectedId) void flush(selectedId, selected?.title ?? "Meeting notes", body);
      else if (body.trim()) void createFirst(body);
    }, AUTOSAVE_MS);

    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, [meetingId, selectedId, body, selected?.title, flush, createFirst]);

  const addAnother = async () => {
    if (!meetingId) return;
    // Flush the current note first, or the pending edit dies with the timer.
    if (selectedId) {
      if (timer.current) clearTimeout(timer.current);
      await flush(selectedId, selected?.title ?? "Meeting notes", body);
    }
    try {
      const note = await api.createMeetingNote(meetingId, {
        title: meetingTitle ? `Notes — ${meetingTitle}` : "Meeting notes",
        body: "",
      });
      setNotes((current) => [note, ...current]);
      setSelectedId(note.id);
      setBody("");
      persisted.current = "";
      setSave("idle");
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not create a note.");
    }
  };

  const open = async (note: Note) => {
    if (selectedId && selectedId !== note.id) {
      if (timer.current) clearTimeout(timer.current);
      await flush(selectedId, selected?.title ?? "Meeting notes", body);
    }
    setSelectedId(note.id);
    setBody(note.body);
    persisted.current = note.body;
    setSave("idle");
  };

  if (!meetingId) {
    return (
      <div className="flex flex-1 items-center justify-center px-6 text-center">
        <p className="text-[13px] text-ink-muted">Select a meeting to take notes on it.</p>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-hairline px-8 py-2">
        {notes.length > 1 && (
          <div className="flex min-w-0 flex-1 gap-1 overflow-x-auto">
            {notes.map((note) => (
              <button
                key={note.id}
                type="button"
                onClick={() => void open(note)}
                className={`shrink-0 rounded-full px-2.5 py-1 text-[11.5px] transition ${
                  note.id === selectedId
                    ? "bg-overlay font-medium text-ink"
                    : "text-ink-muted hover:bg-overlay hover:text-ink"
                }`}
              >
                {relativeTime(note.created_at)}
              </button>
            ))}
          </div>
        )}
        {notes.length <= 1 && <span className="flex-1" />}

        <span role="status" className={`text-[11px] ${save === "failed" ? "text-danger-text" : "text-ink-faint"}`}>
          {save === "saving"
            ? "Saving…"
            : save === "saved"
              ? "Saved"
              : save === "failed"
                ? "Not saved"
                : ""}
        </span>

        {selected && (
          <button
            type="button"
            onClick={() => onNavigate({ name: "notes", id: selected.id })}
            title="Open in Notes"
            className="flex shrink-0 items-center gap-1 rounded-full border border-hairline px-2 py-1
                       text-[11.5px] text-ink-muted transition hover:bg-overlay hover:text-ink"
          >
            <ExternalLink size={11} aria-hidden />
            Open
          </button>
        )}

        <button
          type="button"
          onClick={() => void addAnother()}
          title="New note on this meeting"
          aria-label="New note on this meeting"
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded text-ink-faint
                     transition hover:bg-overlay hover:text-ink"
        >
          <Plus size={14} aria-hidden />
        </button>
      </div>

      {loading ? (
        <p className="flex flex-1 items-center justify-center gap-2 text-[12.5px] text-ink-faint">
          <Loader2 size={14} className="animate-spin" aria-hidden />
          Loading
        </p>
      ) : (
        <textarea
          value={body}
          onChange={(event) => setBody(event.target.value)}
          onBlur={() => {
            if (selectedId) void flush(selectedId, selected?.title ?? "Meeting notes", body);
            else if (body.trim()) void createFirst(body);
          }}
          placeholder={
            isRecording
              ? "Type while it runs. Saved automatically."
              : "Your own notes on this meeting. Markdown, saved automatically."
          }
          aria-label="Meeting notes"
          className="min-h-0 flex-1 resize-none bg-transparent px-8 py-5 text-[14px]
                     leading-relaxed text-ink outline-none placeholder:text-ink-faint"
        />
      )}

      {error && (
        <p role="status" className="border-t border-hairline px-8 py-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </div>
  );
}
