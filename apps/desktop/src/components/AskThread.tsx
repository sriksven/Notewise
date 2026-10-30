import { useEffect, useRef, type ReactNode } from "react";
import { AlertTriangle, Loader2, Send, Sparkles } from "lucide-react";

import { citationRoute, notGroundedNote, type AskThread as Thread } from "../lib/ask";
import type { Route } from "../lib/router";

/**
 * Which colour a conversation is drawn in.
 *
 * `workspace` is the app's accent. `note` is its companion — another of the eleven, chosen in
 * `lib/theme` — because asking one note and asking everything sit side by side often enough that
 * they should never look like the same thing. The class strings are spelled out in full so Tailwind
 * finds them; a class assembled at runtime generates no CSS.
 */
const TONES = {
  workspace: {
    question: "bg-accent text-accent-on",
    send: "bg-accent text-accent-on hover:bg-accent-hover",
    focus: "focus:border-accent",
    suggestion: "hover:border-accent/60",
  },
  note: {
    question: "bg-note text-note-on",
    send: "bg-note text-note-on hover:bg-note-hover",
    focus: "focus:border-note",
    suggestion: "hover:border-note/60",
  },
} as const;

export type AskTone = keyof typeof TONES;

interface Props {
  thread: Thread;
  tone: AskTone;
  /** Shown before the first question: what this one searches. */
  intro: ReactNode;
  /** Offered before the first question, as one-click questions. */
  suggestions?: string[];
  placeholder: string;
  onNavigate: (route: Route) => void;
  /** `page` centres a wide column; `panel` fills a side panel. */
  layout?: "page" | "panel";
}

/** A conversation with your own material: questions, cited answers, and the box to ask in. */
export function AskThread({
  thread,
  tone,
  intro,
  suggestions = [],
  placeholder,
  onNavigate,
  layout = "panel",
}: Props) {
  const colours = TONES[tone];
  const endRef = useRef<HTMLDivElement>(null);
  const { turns, draft, setDraft, busy, error, send } = thread;

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [turns.length, busy]);

  const column = layout === "page" ? "mx-auto max-w-2xl" : "";
  const gutter = layout === "page" ? "px-8" : "px-4";

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className={`min-h-0 flex-1 overflow-y-auto ${gutter} py-4`}>
        <div className={`${column} space-y-4`}>
          {turns.length === 0 && (
            <>
              <div className="text-[12.5px] leading-relaxed text-ink-muted">{intro}</div>
              {suggestions.length > 0 && (
                <div className="space-y-1.5">
                  {suggestions.map((suggestion) => (
                    <button
                      key={suggestion}
                      type="button"
                      onClick={() => void send(suggestion)}
                      className={`card w-full px-3 py-2 text-left text-[12.5px] text-ink-muted transition
                                  hover:bg-overlay hover:text-ink ${colours.suggestion}`}
                    >
                      {suggestion}
                    </button>
                  ))}
                </div>
              )}
            </>
          )}

          {turns.map((turn, index) => (
            <div key={index} className="space-y-1.5">
              <div
                className={`ml-6 rounded-xl px-3 py-2 text-[12.5px] leading-relaxed ${colours.question}`}
              >
                {turn.question}
              </div>

              {!turn.answer ? (
                <p className="flex items-center gap-2 px-1 text-[12px] text-ink-faint">
                  <Loader2 size={12} className="animate-spin" aria-hidden />
                  Reading what you have
                </p>
              ) : (
                <div>
                  {!turn.answer.grounded && (
                    <p className="mb-1.5 flex items-start gap-1.5 rounded-lg border border-warn-line
                                  bg-warn px-3 py-2 text-[11.5px] leading-relaxed text-warn-text">
                      <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
                      {notGroundedNote(turn.answer)}
                    </p>
                  )}

                  <div className="whitespace-pre-wrap rounded-xl border border-hairline bg-surface px-3 py-2
                                  text-[12.5px] leading-relaxed text-ink">
                    {turn.answer.text}
                  </div>

                  {turn.answer.citations.length > 0 && (
                    <ul className="mt-1.5 space-y-0.5">
                      {turn.answer.citations.map((citation) => {
                        const to = citationRoute(citation);
                        return (
                          <li key={citation.n}>
                            <button
                              type="button"
                              onClick={() => to && onNavigate(to)}
                              disabled={!to}
                              title={to ? `Open ${citation.title}` : undefined}
                              className="flex w-full items-baseline gap-1.5 rounded px-1 py-0.5 text-left
                                         text-[11.5px] text-ink-muted transition enabled:hover:bg-overlay
                                         enabled:hover:text-ink"
                            >
                              <span className="shrink-0 font-mono text-ink-faint">[{citation.n}]</span>
                              <span className="truncate">{citation.title}</span>
                              <span className="shrink-0 text-ink-faint">{citation.kind}</span>
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}

                  <p className="mt-1 flex items-center gap-1 px-1 text-[10.5px] text-ink-faint">
                    <Sparkles size={9} aria-hidden />
                    {turn.answer.model}
                  </p>
                </div>
              )}
            </div>
          ))}

          {error && (
            <p role="alert" className="text-[12px] text-danger-text">
              {error}
            </p>
          )}

          <div ref={endRef} />
        </div>
      </div>

      <div className={`border-t border-hairline ${gutter} py-3`}>
        <div className={`${column} flex items-end gap-2`}>
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              // Enter asks; Shift+Enter is a new line, for a question that needs two.
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
            rows={1}
            placeholder={placeholder}
            aria-label="Your question"
            className={`max-h-28 flex-1 resize-none rounded-lg border border-hairline bg-surface px-2.5
                        py-1.5 text-[12.5px] text-ink outline-none transition placeholder:text-ink-faint
                        ${colours.focus}`}
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={busy || draft.trim().length === 0}
            aria-label="Ask"
            className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-full transition
                        disabled:bg-hairline disabled:text-ink-faint ${colours.send}`}
          >
            {busy ? <Loader2 size={13} className="animate-spin" aria-hidden /> : <Send size={13} aria-hidden />}
          </button>
        </div>
      </div>
    </div>
  );
}
