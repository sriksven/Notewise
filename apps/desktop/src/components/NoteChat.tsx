import { useEffect, useRef, useState } from "react";
import { Globe, Loader2, Send, X } from "lucide-react";

import { api, ApiError, type Citation } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  noteId: string;
  noteTitle: string;
  onClose: () => void;
  onNavigate: (route: Route) => void;
}

interface Turn {
  role: "user" | "assistant";
  content: string;
  citations?: Citation[];
  /** False when the engine found no material at all, which is worth explaining. */
  grounded?: boolean;
}

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
 */
export function NoteChat({ noteId, noteTitle, onClose, onNavigate }: Props) {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [draft, setDraft] = useState("");
  const [wide, setWide] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  // Switching notes must not carry the thread across — the answers would be grounded in
  // material the user is no longer looking at.
  useEffect(() => {
    setTurns([]);
    setError(null);
  }, [noteId]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [turns.length, busy]);

  const send = async () => {
    const question = draft.trim();
    if (!question || busy) return;

    const next: Turn[] = [...turns, { role: "user", content: question }];
    setTurns(next);
    setDraft("");
    setBusy(true);
    setError(null);

    try {
      const answer = await api.askNote(
        noteId,
        next.map((turn) => ({ role: turn.role, content: turn.content })),
        wide ? "workspace" : "note",
      );
      setTurns([
        ...next,
        {
          role: "assistant",
          content: answer.text,
          citations: answer.citations,
          grounded: answer.grounded,
        },
      ]);
    } catch (e) {
      // The question stays in the thread so it can be retried without retyping.
      setError(e instanceof ApiError ? e.message : "Could not get an answer.");
    } finally {
      setBusy(false);
    }
  };

  const openCitation = (citation: Citation) => {
    if (citation.kind === "meeting") {
      onNavigate({ name: "meeting", id: citation.id, tab: "transcript" });
    } else if (citation.kind === "note") {
      onNavigate({ name: "notes", id: citation.id });
    } else {
      onNavigate({ name: "tickets" });
    }
  };

  return (
    <aside className="flex w-[340px] shrink-0 flex-col border-l border-hairline bg-rail">
      <header className="flex items-center gap-2 border-b border-hairline px-4 py-2.5">
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
          className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--accent)]"
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

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        {turns.length === 0 && (
          <p className="text-[12px] leading-relaxed text-ink-faint">
            Answers come only from your own material, and say which part they came from.
            Matching is by word, so use terms that appear in the text.
          </p>
        )}

        <div className="space-y-3">
          {turns.map((turn, index) => (
            <div key={index}>
              <div
                className={`rounded-xl px-3 py-2 text-[12.5px] leading-relaxed ${
                  turn.role === "user"
                    ? "ml-6 bg-accent text-accent-on"
                    : "border border-hairline bg-surface text-ink"
                }`}
              >
                {turn.content}
              </div>

              {turn.role === "assistant" && turn.citations && turn.citations.length > 0 && (
                <ul className="mt-1.5 space-y-1">
                  {turn.citations.map((citation) => (
                    <li key={citation.n}>
                      <button
                        type="button"
                        onClick={() => openCitation(citation)}
                        className="flex w-full items-baseline gap-1.5 rounded px-1 py-0.5 text-left
                                   text-[11.5px] text-ink-muted transition hover:bg-overlay hover:text-ink"
                      >
                        <span className="shrink-0 font-mono text-ink-faint">
                          [{citation.n}]
                        </span>
                        <span className="truncate">{citation.title}</span>
                        <span className="shrink-0 text-ink-faint">{citation.kind}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}

              {turn.role === "assistant" && turn.grounded === false && (
                <p className="mt-1 text-[11px] leading-snug text-ink-faint">
                  Nothing matched. Retrieval is by word — try the wording that would actually
                  appear.
                </p>
              )}
            </div>
          ))}

          {busy && (
            <p className="flex items-center gap-2 text-[12px] text-ink-faint">
              <Loader2 size={12} className="animate-spin" aria-hidden />
              Reading
            </p>
          )}

          {error && (
            <p role="alert" className="text-[12px] text-danger-text">
              {error}
            </p>
          )}

          <div ref={endRef} />
        </div>
      </div>

      <div className="border-t border-hairline p-3">
        <div className="flex items-end gap-2">
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
            rows={1}
            placeholder="Ask about this note…"
            aria-label="Your question"
            className="max-h-28 flex-1 resize-none rounded-lg border border-hairline bg-surface
                       px-2.5 py-1.5 text-[12.5px] text-ink outline-none transition
                       placeholder:text-ink-faint focus:border-accent"
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={busy || draft.trim().length === 0}
            aria-label="Send"
            className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full
                       bg-accent text-accent-on transition hover:bg-accent-hover
                       disabled:bg-hairline disabled:text-ink-faint"
          >
            <Send size={13} aria-hidden />
          </button>
        </div>
      </div>
    </aside>
  );
}
