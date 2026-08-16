import { useCallback, useEffect, useRef, useState } from "react";
import { FileText, MessageCircleQuestion, Plus, Trash2 } from "lucide-react";

import { NoteChat } from "../components/NoteChat";
import { api, ApiError, type Note } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  /** Which note the address bar says is open, if any. */
  noteId?: string;
  onNavigate: (route: Route) => void;
}

/**
 * How long to wait after the last keystroke before saving.
 *
 * Long enough that ordinary typing produces one save rather than one per word, short enough
 * that closing the window a second after typing does not lose the sentence. Blur and
 * switching notes both flush immediately, so this only governs the idle case.
 */
const AUTOSAVE_MS = 800;

type SaveState = "idle" | "saving" | "saved" | "failed";

/**
 * The workspace's notes.
 *
 * `body` is opaque to the engine — storage treats it as text precisely so the editor format
 * can change without a migration. This is a plain Markdown textarea for now; the roadmap's
 * block editor can replace it without the schema noticing.
 *
 * Autosaves rather than asking the user to press save. A notes pane that can lose work is one
 * people stop trusting with anything that matters, and an explicit save button is a promise
 * to remember something the user should not have to.
 */
export function NotesView({ noteId, onNavigate }: Props) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [save, setSave] = useState<SaveState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);

  /** The last content committed to the engine, so an idle timer can skip a no-op save. */
  const persisted = useRef<{ title: string; body: string }>({ title: "", body: "" });
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const load = useCallback(async () => {
    try {
      const loaded = await api.notes();
      setNotes(loaded);
      setError(null);
      return loaded;
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load notes.");
      return [];
    }
  }, []);

  // Open whatever the address bar names, once the list has arrived. This is how a citation, an
  // agent's finished note, and a link from a meeting all land on the right note — and how a
  // reload stays on it.
  useEffect(() => {
    let cancelled = false;
    void load().then((loaded) => {
      if (cancelled || !noteId) return;
      const wanted = loaded.find((note) => note.id === noteId);
      if (!wanted) return;
      setSelectedId(wanted.id);
      setTitle(wanted.title);
      setBody(wanted.body);
      persisted.current = { title: wanted.title, body: wanted.body };
      setSave("idle");
    });
    return () => {
      cancelled = true;
    };
  }, [load, noteId]);

  const flush = useCallback(async (id: string, nextTitle: string, nextBody: string) => {
    if (
      nextTitle === persisted.current.title &&
      nextBody === persisted.current.body
    ) {
      return;
    }

    setSave("saving");
    try {
      const saved = await api.updateNote(id, nextTitle || "Untitled", nextBody);
      persisted.current = { title: saved.title, body: saved.body };
      setNotes((current) => current.map((n) => (n.id === id ? saved : n)));
      setSave("saved");
      setError(null);
    } catch (e) {
      setSave("failed");
      setError(e instanceof ApiError ? e.message : "Could not save this note.");
    }
  }, []);

  // Debounced autosave. Cleared on every change, so only an idle pause triggers a write.
  useEffect(() => {
    if (!selectedId) return;
    if (timer.current) clearTimeout(timer.current);

    timer.current = setTimeout(() => {
      void flush(selectedId, title, body);
    }, AUTOSAVE_MS);

    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, [selectedId, title, body, flush]);

  async function open(note: Note) {
    // Flush the outgoing note before switching, or the pending edit is lost with the timer.
    if (selectedId && selectedId !== note.id) {
      if (timer.current) clearTimeout(timer.current);
      await flush(selectedId, title, body);
    }

    setSelectedId(note.id);
    setTitle(note.title);
    setBody(note.body);
    persisted.current = { title: note.title, body: note.body };
    setSave("idle");
    // Keep the address bar honest, so reload and Back both work on a note.
    onNavigate({ name: "notes", id: note.id });
  }

  async function create() {
    try {
      const note = await api.createNote({ title: "Untitled", body: "" });
      setNotes((current) => [note, ...current]);
      await open(note);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not create a note.");
    }
  }

  /**
   * Move a note to the trash.
   *
   * No longer asks first, because it no longer destroys anything — the note is recoverable
   * from the trash, and a confirm dialog in front of a reversible action is the one people
   * learn to click through without reading.
   */
  async function remove(note: Note) {
    try {
      await api.deleteNote(note.id);
      setNotes((current) => current.filter((n) => n.id !== note.id));
      if (selectedId === note.id) {
        // The pending autosave would be rejected by the engine now, but cancelling it keeps
        // a spurious "Not saved" off the screen.
        if (timer.current) clearTimeout(timer.current);
        setSelectedId(null);
        setTitle("");
        setBody("");
        setAsking(false);
        onNavigate({ name: "notes" });
      }
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not delete that note.");
    }
  }

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-[240px] shrink-0 flex-col border-r border-hairline">
        <div className="flex items-center justify-between border-b border-hairline px-4 py-3">
          <h2 className="text-[13px] font-semibold text-ink">Notes</h2>
          <button
            type="button"
            onClick={() => void create()}
            aria-label="New note"
            title="New note"
            className="flex h-6 w-6 items-center justify-center rounded text-ink-faint
                       transition hover:bg-overlay hover:text-ink"
          >
            <Plus size={14} aria-hidden />
          </button>
        </div>

        {notes.length === 0 ? (
          <p className="px-4 py-3 text-[12px] leading-relaxed text-ink-faint">
            No notes yet. A note is a place to think — meeting pages will land here too.
          </p>
        ) : (
          <ul className="min-h-0 flex-1 overflow-y-auto py-1">
            {notes.map((note) => (
              <li key={note.id}>
                <div
                  className={`group flex items-center gap-1 px-2 ${
                    selectedId === note.id ? "bg-overlay" : ""
                  }`}
                >
                  <button
                    type="button"
                    onClick={() => void open(note)}
                    className="min-w-0 flex-1 rounded px-2 py-1.5 text-left"
                  >
                    <span className="block truncate text-[12.5px] text-ink">
                      {note.title || "Untitled"}
                    </span>
                    <span className="block text-[10.5px] text-ink-faint">
                      {new Date(note.updated_at).toLocaleDateString([], {
                        month: "short",
                        day: "numeric",
                      })}
                    </span>
                  </button>
                  <button
                    type="button"
                    onClick={() => void remove(note)}
                    aria-label={`Move ${note.title || "Untitled"} to the trash`}
                    title="Move to trash"
                    className="shrink-0 p-1 text-ink-faint opacity-0 transition
                               hover:text-danger-text group-hover:opacity-100
                               focus-visible:opacity-100"
                  >
                    <Trash2 size={12} aria-hidden />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        {!selectedId ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-2 text-ink-faint">
            <FileText size={28} strokeWidth={1.5} aria-hidden />
            <p className="text-[12.5px]">Select a note, or make a new one.</p>
          </div>
        ) : (
          <>
            <div className="flex items-center gap-3 border-b border-hairline px-5 py-2.5">
              <input
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                onBlur={() => void flush(selectedId, title, body)}
                placeholder="Untitled"
                aria-label="Note title"
                className="min-w-0 flex-1 bg-transparent text-[14px] font-semibold
                           text-ink outline-none placeholder:text-ink-faint"
              />
              {/* Saving state is stated rather than implied. A pane that autosaves silently
                  gives a user no way to know whether their work is safe, and the failed case
                  is the one that matters. */}
              <span
                role="status"
                className={`shrink-0 text-[11px] ${
                  save === "failed" ? "text-danger-text" : "text-ink-faint"
                }`}
              >
                {save === "saving"
                  ? "Saving…"
                  : save === "saved"
                    ? "Saved"
                    : save === "failed"
                      ? "Not saved"
                      : ""}
              </span>

              <button
                type="button"
                onClick={() => setAsking((open) => !open)}
                aria-pressed={asking}
                // Labelled, not just titled: the visible word is "Ask", which read on its own
                // by a screen reader does not say what is being asked.
                aria-label="Ask about this note"
                title="Ask about this note"
                className={`flex shrink-0 items-center gap-1 rounded-full border border-hairline
                            px-2.5 py-1 text-[11.5px] transition hover:bg-overlay ${
                              asking ? "bg-overlay text-ink" : "text-ink-muted hover:text-ink"
                            }`}
              >
                <MessageCircleQuestion size={12} aria-hidden />
                Ask
              </button>
            </div>

            <textarea
              value={body}
              onChange={(e) => setBody(e.target.value)}
              onBlur={() => void flush(selectedId, title, body)}
              placeholder="Markdown."
              aria-label="Note body"
              className="min-h-0 flex-1 resize-none bg-transparent px-5 py-4 text-[13px]
                         leading-relaxed text-ink outline-none
                         placeholder:text-ink-faint"
            />
          </>
        )}

        {error && (
          <p role="status" className="border-t border-hairline px-5 py-2 text-[12px] text-danger-text">
            {error}
          </p>
        )}
      </div>

      {asking && selectedId && (
        <NoteChat
          // Keyed on the note, so switching notes resets the thread rather than carrying
          // answers grounded in material the user is no longer looking at.
          key={selectedId}
          noteId={selectedId}
          noteTitle={title}
          onClose={() => setAsking(false)}
          onNavigate={onNavigate}
        />
      )}
    </div>
  );
}
