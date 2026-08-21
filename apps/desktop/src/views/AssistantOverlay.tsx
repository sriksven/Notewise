import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, ArrowRight, Loader2, ScanText, Sparkles, Wand2 } from "lucide-react";

import {
  api,
  ApiError,
  type ActResult,
  type AssistantAction,
  type ScreenAnswer,
  type SelectionInfo,
} from "../lib/api";

/**
 * The assistant panel: ask about what is on screen, or act on what is highlighted.
 *
 * Specs 9b and 9c in one surface, because they are one gesture from the user's side — the hotkey
 * opens this, and what it offers depends on whether anything is selected. Two windows for "ask" and
 * "rewrite" would be two hotkeys to remember for the same intent.
 *
 * # Why what was sent is shown
 *
 * This is the only surface in Notewise that reads a window belonging to another application. A user
 * is entitled to see exactly what left that window, so the context is available verbatim rather than
 * described — and the answer says plainly when it was grounded in nothing.
 */
export function AssistantOverlay() {
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<ScreenAnswer | null>(null);
  const [selection, setSelection] = useState<SelectionInfo | null>(null);
  const [actions, setActions] = useState<AssistantAction[]>([]);
  const [acted, setActed] = useState<ActResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showContext, setShowContext] = useState(false);

  const load = useCallback(async () => {
    const [found, list] = await Promise.all([
      api.currentSelection().catch(() => null),
      api.assistantActions().catch(() => []),
    ]);
    setSelection(found);
    setActions(list);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const ask = async () => {
    if (!question.trim()) return;
    setBusy(true);
    setError(null);
    setActed(null);
    try {
      setAnswer(await api.askAboutScreen(question));
    } catch (e) {
      // A refused grant comes back as a conflict with the pane to open in it, so the message is
      // already the right thing to show.
      setError(e instanceof ApiError ? e.message : "Could not answer that.");
    } finally {
      setBusy(false);
    }
  };

  const act = async (action: AssistantAction) => {
    setBusy(true);
    setError(null);
    setAnswer(null);
    try {
      setActed(
        await api.actOnSelection({
          action: action.action,
          // Replace only where it means something and where the target will take it.
          replace: action.replaces && (selection?.replaceable ?? false),
        }),
      );
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not do that.");
    } finally {
      setBusy(false);
    }
  };

  const hasSelection = Boolean(selection?.text?.trim());

  return (
    <div className="flex h-screen flex-col bg-surface">
      <header className="flex items-center gap-2 border-b border-hairline px-3 py-2">
        <Sparkles size={13} className="shrink-0 text-ink-faint" aria-hidden />
        <span className="text-[12.5px] font-medium text-ink">Notewise</span>
        <span className="flex-1 truncate text-[11.5px] text-ink-faint">
          {hasSelection
            ? `${selection?.length ?? 0} characters selected`
            : "asks about the window in front"}
        </span>
      </header>

      <div className="flex items-center gap-2 border-b border-hairline px-3 py-2">
        <input
          value={question}
          onChange={(event) => setQuestion(event.target.value)}
          onKeyDown={(event) => event.key === "Enter" && void ask()}
          placeholder="Ask about what you are looking at"
          autoFocus
          className="min-w-0 flex-1 bg-transparent text-[13px] text-ink outline-none
                     placeholder:text-ink-faint"
        />
        <button
          type="button"
          onClick={() => void ask()}
          disabled={busy || !question.trim()}
          aria-label="Ask"
          className="shrink-0 rounded-full bg-accent p-1.5 text-accent-on transition
                     hover:opacity-90 disabled:opacity-40"
        >
          {busy ? (
            <Loader2 size={12} className="animate-spin" aria-hidden />
          ) : (
            <ArrowRight size={12} aria-hidden />
          )}
        </button>
      </div>

      {/* Quick actions appear only when there is something to act on, and "replace" only where it
          can work — offering it otherwise costs the user their selection for nothing. */}
      {hasSelection && actions.length > 0 && (
        <div className="flex flex-wrap gap-1.5 border-b border-hairline px-3 py-2">
          {actions.map((action) => (
            <button
              key={String(action.action)}
              type="button"
              onClick={() => void act(action)}
              disabled={busy}
              className="flex items-center gap-1 rounded-full border border-hairline px-2 py-0.5
                         text-[11.5px] text-ink-muted transition hover:bg-overlay hover:text-ink
                         disabled:opacity-50"
            >
              <Wand2 size={10} aria-hidden />
              {action.label}
              {action.replaces && !selection?.replaceable && (
                <span className="text-ink-faint" title="This app will not accept a replacement">
                  ·copy
                </span>
              )}
            </button>
          ))}
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2.5">
        {error && (
          <p
            role="alert"
            className="flex items-start gap-1.5 rounded border border-warn-line bg-warn px-2 py-1.5
                       text-[12px] leading-relaxed text-warn-text"
          >
            <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
            {error}
          </p>
        )}

        {answer && (
          <div>
            <p className="whitespace-pre-wrap text-[13px] leading-relaxed text-ink">
              {answer.text}
            </p>

            {!answer.grounded && (
              <p className="mt-2 text-[11.5px] text-ink-faint">
                Nothing readable was on screen, so this was answered without it.
              </p>
            )}

            {answer.context_prompt && (
              <div className="mt-2.5">
                <button
                  type="button"
                  onClick={() => setShowContext((current) => !current)}
                  className="flex items-center gap-1 text-[11.5px] text-ink-faint transition hover:text-ink"
                >
                  <ScanText size={11} aria-hidden />
                  {showContext ? "Hide" : "Show"} what was sent
                </button>
                {showContext && (
                  <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap rounded bg-overlay
                                  p-2 font-mono text-[11px] leading-snug text-ink-muted">
                    {answer.context_prompt}
                  </pre>
                )}
              </div>
            )}
          </div>
        )}

        {acted && (
          <div>
            <p className="whitespace-pre-wrap text-[13px] leading-relaxed text-ink">
              {acted.result}
            </p>
            {acted.note && (
              <p className="mt-1.5 text-[11.5px] leading-relaxed text-ink-faint">{acted.note}</p>
            )}
            {!acted.note && acted.insertion === null && (
              <p className="mt-1.5 text-[11.5px] text-ink-faint">
                Your original text is untouched — copy this if you want it.
              </p>
            )}
          </div>
        )}

        {!answer && !acted && !error && (
          <p className="pt-4 text-center text-[12px] leading-relaxed text-ink-faint">
            {hasSelection
              ? "Pick an action for what you highlighted, or ask a question about it."
              : "Highlight something first for quick actions, or just ask."}
          </p>
        )}
      </div>
    </div>
  );
}
