import { useCallback, useState } from "react";
import { FileText, Globe, X } from "lucide-react";

import { api } from "../lib/api";
import { useAskThread, type AskMessage } from "../lib/ask";
import type { Route } from "../lib/router";
import { AskThread } from "./AskThread";

interface Props {
  noteId: string;
  noteTitle: string;
  onClose: () => void;
  onNavigate: (route: Route) => void;
}

const NOTE_SUGGESTIONS = ["What are the open questions here?", "Summarise this note in three lines."];

/**
 * Ask a note questions.
 *
 * Two scopes, because they answer different questions. *This note* grounds only on what is in
 * front of you — good for "what did I mean by that", and it cannot drag in a coincidence from
 * an unrelated meeting. *Whole workspace* searches everything and keeps the note as the first
 * citation, which is how you ask a note what the meetings said about it.
 *
 * Answers carry their citations and the citations are clickable. That is the whole safety
 * property: without a way back to the source, a grounded answer and an invented one look
 * identical.
 *
 * The conversation itself is the one every Ask uses (`lib/ask`, `AskThread`), so it has the same
 * features — follow-ups, suggestions of what to ask, citations that open, the note when nothing
 * answers. It is drawn in the accent's companion colour, not the accent: this asks one note, the
 * Ask everywhere else asks everything, and the two should never look like the same thing.
 */
export function NoteChat({ noteId, noteTitle, onClose, onNavigate }: Props) {
  const [wide, setWide] = useState(false);
  const ask = useCallback(
    (messages: AskMessage[]) => api.askNote(noteId, messages, wide ? "workspace" : "note"),
    [noteId, wide],
  );
  // Switching notes starts a new thread because `NotesView` keys this panel on the note.
  const thread = useAskThread(ask);

  return (
    <aside className="flex w-[340px] shrink-0 flex-col border-l border-hairline bg-rail">
      <header className="flex items-center gap-2 border-b border-hairline px-4 py-2.5">
        <FileText size={13} className="shrink-0 text-note" aria-hidden />
        <h2 className="flex-1 truncate text-[12.5px] font-semibold text-ink">
          Ask “{noteTitle || "Untitled"}”
        </h2>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close"
          className="text-ink-faint transition hover:text-ink"
        >
          <X size={14} aria-hidden />
        </button>
      </header>

      <label className="flex cursor-pointer items-start gap-2 border-b border-hairline px-4 py-2">
        <input
          type="checkbox"
          checked={wide}
          onChange={(event) => setWide(event.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--note-accent)]"
        />
        <span className="min-w-0">
          <span className="flex items-center gap-1 text-[12px] font-medium text-ink">
            <Globe size={11} aria-hidden />
            Search the whole workspace
          </span>
          <span className="mt-0.5 block text-[11px] leading-snug text-ink-faint">
            Also draws on your meetings and other notes.
          </span>
        </span>
      </label>

      <AskThread
        thread={thread}
        tone="note"
        suggestions={NOTE_SUGGESTIONS}
        placeholder={wide ? "Ask this note and everything else…" : "Ask about this note…"}
        onNavigate={onNavigate}
        intro={
          wide
            ? "Answers come from this note and the rest of your workspace, and say which part they came from."
            : "Answers come only from this note, and say which part they came from."
        }
      />
    </aside>
  );
}
