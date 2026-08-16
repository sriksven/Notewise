import { useCallback, useEffect, useRef, useState } from "react";
import { FileText, Plus, Trash2 } from "lucide-react";

import { api, ApiError, type Note } from "../lib/api";

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
export function NotesView() {
  const [notes, setNotes] = useState<Note[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [save, setSave] = useState<SaveState>("idle");
  const [error, setError] = useState<string | null>(null);

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

  useEffect(() => {
    void load();
  }, [load]);

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

  async function remove(note: Note) {
    // Deliberate: this is the one destructive action in the view, and it asks first. There
    // is no undo behind it, and a note is often the only copy of what someone typed.
    if (!window.confirm(`Delete “${note.title}”? This cannot be undone.`)) return;

    try {
      await api.deleteNote(note.id);
      setNotes((current) => current.filter((n) => n.id !== note.id));
      if (selectedId === note.id) {
        if (timer.current) clearTimeout(timer.current);
        setSelectedId(null);
        setTitle("");
        setBody("");
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
                    aria-label={`Delete ${note.title || "Untitled"}`}
                    className="shrink-0 p-1 text-ink-faint opacity-0 transition
                               hover:text-danger-text group-hover:opacity-100"
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
    </div>
  );
}
