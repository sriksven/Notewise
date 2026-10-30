import { useCallback } from "react";
import { Search, X } from "lucide-react";

import { api } from "../lib/api";
import { SUGGESTIONS, useAskThread, type AskMessage } from "../lib/ask";
import type { Route } from "../lib/router";
import { AskThread } from "./AskThread";

interface Props {
  onClose: () => void;
  onNavigate: (route: Route) => void;
}

/**
 * Ask everything, from wherever you are.
 *
 * The same conversation as the Ask page, in a side panel, so a question does not cost leaving the
 * meeting it came up in. It stays open as you move around — opening a citation shows its source
 * beside the answer that cited it, which is the point of a citation.
 */
export function AskPanel({ onClose, onNavigate }: Props) {
  const ask = useCallback((messages: AskMessage[]) => api.ask(messages), []);
  const thread = useAskThread(ask);

  return (
    <aside
      aria-label="Ask your workspace"
      className="flex w-[360px] shrink-0 flex-col border-l border-hairline bg-rail"
    >
      <header className="flex items-center gap-2 border-b border-hairline px-4 py-2.5">
        <Search size={13} className="shrink-0 text-accent" aria-hidden />
        <h2 className="flex-1 truncate text-[12.5px] font-semibold text-ink">Ask</h2>
        <span className="text-[11px] text-ink-faint">everything you have</span>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close Ask"
          className="ml-1 text-ink-faint transition hover:text-ink"
        >
          <X size={14} aria-hidden />
        </button>
      </header>

      <AskThread
        thread={thread}
        tone="workspace"
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
    </aside>
  );
}
