import { useRef, useState } from "react";
import { AlertTriangle, Loader2, Search, Send, Sparkles } from "lucide-react";

import { api, ApiError, type Citation, type GroundedAnswer } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  onNavigate: (route: Route) => void;
}

interface Turn {
  question: string;
  answer: GroundedAnswer | null;
}

const SUGGESTIONS = [
  "What did we decide about pricing?",
  "What am I supposed to be doing this week?",
  "Who has raised concerns about the timeline?",
];

/**
 * Ask everything at once.
 *
 * # Why this is a screen and not part of search
 *
 * Search answers "where was this said" and this answers "what is the answer". They fail differently:
 * a search with no hits is a list you can widen, and a question with no material behind it needs to
 * say so in words. The library's search box is a 268px rail, which is no place for a paragraph with
 * four citations under it.
 *
 * A per-meeting version already existed on the meeting's Ask tab, and a per-note one in `NoteChat`.
 * The workspace-wide endpoint was the one with no surface — the broadest of the three, and the only
 * one nobody could reach.
 *
 * # Citations are the whole point
 *
 * The answer comes only from what retrieval found, and every claim carries a `[n]` pointing at a
 * meeting, note or ticket you can open. That is what makes it checkable rather than plausible. When
 * retrieval found nothing the engine says so with `grounded: false`, and this shows that instead of
 * quietly presenting an answer built on nothing — retrieval is by word, so a rewording may find what
 * the first attempt missed.
 */
export function AskView({ onNavigate }: Props) {
  const [question, setQuestion] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  const ask = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || busy) return;

    setBusy(true);
    setError(null);
    setQuestion("");
    setTurns((current) => [...current, { question: trimmed, answer: null }]);

    try {
      // The whole exchange so far, so a follow-up like "and who owns that" has something to refer
      // to. Only the questions and the answers' text — a citation list is not something to answer.
      const messages = [
        ...turns.flatMap((turn) =>
          turn.answer
            ? [
                { role: "user", content: turn.question },
                { role: "assistant", content: turn.answer.text },
              ]
            : [],
        ),
        { role: "user", content: trimmed },
      ];
      const answer = await api.ask(messages);
      setTurns((current) =>
        current.map((turn, i) => (i === current.length - 1 ? { ...turn, answer } : turn)),
      );
      requestAnimationFrame(() => endRef.current?.scrollIntoView({ behavior: "smooth" }));
    } catch (e) {
      // The failed question is dropped rather than left hanging with no answer under it.
      setTurns((current) => current.slice(0, -1));
      setQuestion(trimmed);
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
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Search size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Ask</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          Everything you have, in one question
        </span>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
        <div className="mx-auto max-w-2xl">
          {turns.length === 0 && (
            <>
              <p className="text-[13px] leading-relaxed text-ink-muted">
                Answered only from your own meetings, notes and tickets — never from what the model
                happens to know. Every claim carries a reference you can open and check.
              </p>

              <div className="mt-6 space-y-1.5">
                {SUGGESTIONS.map((suggestion) => (
                  <button
                    key={suggestion}
                    type="button"
                    onClick={() => void ask(suggestion)}
                    className="card w-full px-4 py-2.5 text-left text-[12.5px] text-ink-muted
                               transition hover:bg-overlay hover:text-ink"
                  >
                    {suggestion}
                  </button>
                ))}
              </div>
            </>
          )}

          <ol className="space-y-5">
            {turns.map((turn, index) => (
              <li key={index}>
                <p className="text-[13px] font-medium leading-snug text-ink">{turn.question}</p>

                {!turn.answer ? (
                  <p className="mt-2 flex items-center gap-2 text-[12.5px] text-ink-faint">
                    <Loader2 size={12} className="animate-spin" aria-hidden />
                    Reading what you have
                  </p>
                ) : (
                  <div className="mt-2">
                    {!turn.answer.grounded && (
                      <p className="mb-2 flex items-start gap-1.5 rounded-lg border border-warn-line
                                    bg-warn px-3 py-2 text-[12px] leading-relaxed text-warn-text">
                        <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
                        Nothing in your workspace matched this. Retrieval is by word, so different
                        wording may find it.
                      </p>
                    )}

                    <p className="whitespace-pre-wrap text-[13px] leading-relaxed text-ink">
                      {turn.answer.text}
                    </p>

                    {turn.answer.citations.length > 0 && (
                      <ul className="mt-2 flex flex-wrap gap-1.5">
                        {turn.answer.citations.map((citation) => (
                          <li key={citation.n}>
                            <button
                              type="button"
                              onClick={() => openCitation(citation)}
                              title={`Open ${citation.title}`}
                              className="flex items-center gap-1 rounded-full border border-hairline
                                         bg-surface px-2 py-0.5 text-[11.5px] text-ink-muted
                                         transition hover:bg-overlay hover:text-ink"
                            >
                              <span className="tabular-nums text-ink-faint">[{citation.n}]</span>
                              <span className="max-w-[220px] truncate">{citation.title}</span>
                            </button>
                          </li>
                        ))}
                      </ul>
                    )}

                    <p className="mt-1.5 flex items-center gap-1 text-[11px] text-ink-faint">
                      <Sparkles size={10} aria-hidden />
                      {turn.answer.model}
                    </p>
                  </div>
                )}
              </li>
            ))}
          </ol>

          <div ref={endRef} />

          {error && (
            <p role="alert" className="mt-3 text-[12px] text-danger-text">
              {error}
            </p>
          )}
        </div>
      </div>

      <form
        onSubmit={(event) => {
          event.preventDefault();
          void ask(question);
        }}
        className="border-t border-hairline px-8 py-3"
      >
        <div className="mx-auto flex max-w-2xl items-center gap-2">
          <input
            value={question}
            onChange={(event) => setQuestion(event.target.value)}
            placeholder="Ask about anything you have recorded"
            aria-label="Your question"
            className="flex-1 rounded-lg border border-hairline bg-surface px-3 py-2 text-[13px]
                       text-ink outline-none placeholder:text-ink-faint"
          />
          <button
            type="submit"
            disabled={busy || !question.trim()}
            aria-label="Ask"
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent
                       text-accent-on transition hover:opacity-90
                       disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy ? (
              <Loader2 size={14} className="animate-spin" aria-hidden />
            ) : (
              <Send size={14} aria-hidden />
            )}
          </button>
        </div>
      </form>
    </div>
  );
}
