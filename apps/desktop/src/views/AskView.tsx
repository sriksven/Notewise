import { useCallback } from "react";
import { Search } from "lucide-react";

import { AskThread } from "../components/AskThread";
import { api } from "../lib/api";
import { SUGGESTIONS, useAskThread, type AskMessage } from "../lib/ask";
import type { Route } from "../lib/router";

// Kept importable from here: tests and older callers reach for it by this path.
export { notGroundedNote } from "../lib/ask";

interface Props {
  onNavigate: (route: Route) => void;
}

/**
 * Ask everything at once.
 *
 * # Why this is a screen and not part of search
 *
 * Search answers "where was this said" and this answers "what is the answer". They fail differently:
 * a search with no hits is a list you can widen, and a question with no material behind it needs to
 * say so in words.
 *
 * The same conversation also opens as a panel from any screen (`AskPanel`) and as a note's own Ask
 * (`NoteChat`). All three are one implementation — `lib/ask` and `AskThread` — so they behave the
 * same and differ only in reach.
 *
 * # Citations are the whole point
 *
 * The answer comes only from what retrieval found, and every claim carries a `[n]` pointing at a
 * meeting or note you can open. That is what makes it checkable rather than plausible. When nothing
 * answers the question, the engine says so with `grounded: false`, and this shows that instead of
 * presenting an answer built on nothing.
 */
export function AskView({ onNavigate }: Props) {
  const ask = useCallback((messages: AskMessage[]) => api.ask(messages), []);
  const thread = useAskThread(ask);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Search size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Ask</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          Everything you have, in one question
        </span>
      </header>

      <AskThread
        thread={thread}
        tone="workspace"
        layout="page"
        suggestions={SUGGESTIONS}
        placeholder="Ask about anything you have recorded"
        onNavigate={onNavigate}
        intro={
          <>
            Answered only from your own meetings and notes — never from what the model happens to
            know. Every claim carries a reference you can open and check.
          </>
        }
      />
    </div>
  );
}
