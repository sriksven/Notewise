import { useEffect, useRef, useState } from "react";
import { Loader2, Send } from "lucide-react";

import { api, ApiError } from "../lib/api";

interface Props {
  meetingId: string | null;
  meetingTitle: string | null;
  hasTranscript: boolean;
}

interface Turn {
  role: "user" | "assistant";
  content: string;
}

/**
 * Ask questions about one meeting, grounded in its transcript.
 *
 * Scoped to a single meeting rather than the whole workspace: grounding on one transcript
 * keeps answers checkable against something the user can read, and cross-meeting search is
 * what the graph and full-text search are for.
 */
export function ChatView({ meetingId, meetingTitle, hasTranscript }: Props) {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  // Reset when the user switches meetings — carrying a conversation across transcripts
  // would produce answers grounded in the wrong material.
  useEffect(() => {
    setTurns([]);
    setError(null);
  }, [meetingId]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [turns.length, busy]);

  const send = async () => {
    const question = draft.trim();
    if (!question || !meetingId || busy) return;

    const next: Turn[] = [...turns, { role: "user", content: question }];
    setTurns(next);
    setDraft("");
    setBusy(true);
    setError(null);

    try {
      const response = await api.chat(meetingId, next);
      setTurns([...next, { role: "assistant", content: response.text }]);
    } catch (e) {
      // The question stays in the thread so the user can retry without retyping.
      setError(e instanceof ApiError ? e.message : "Could not get an answer.");
    } finally {
      setBusy(false);
    }
  };

  if (!meetingId) {
    return (
      <div className="flex flex-1 items-center justify-center px-6 text-center">
        <p className="text-[13px] text-ink-muted">
          Select a meeting to ask questions about it.
        </p>
      </div>
    );
  }

  if (!hasTranscript) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center px-6 text-center">
        <p className="text-[13px] text-ink-muted">
          <strong className="text-ink">{meetingTitle}</strong> has no transcript yet.
        </p>
        <p className="mt-1 text-[12px] text-ink-faint">
          There is nothing to ground an answer in.
        </p>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex-1 overflow-y-auto px-8 py-6">
        <div className="mx-auto max-w-2xl space-y-4">
          {turns.length === 0 && (
            <div className="text-center">
              <p className="text-[13px] text-ink-muted">
                Ask about <strong className="text-ink">{meetingTitle}</strong>
              </p>
              <p className="mt-1 text-[12px] text-ink-faint">
                Answers come only from this meeting's transcript.
              </p>
            </div>
          )}

          {turns.map((turn, index) => (
            <div
              key={index}
              className={turn.role === "user" ? "flex justify-end" : "flex justify-start"}
            >
              <div
                className={`max-w-[85%] rounded-2xl px-3.5 py-2 text-[14px] leading-relaxed ${
                  turn.role === "user"
                    ? "bg-accent text-accent-on"
                    : "border border-hairline bg-surface text-ink"
                }`}
              >
                {turn.content}
              </div>
            </div>
          ))}

          {busy && (
            <div className="flex justify-start">
              <div className="flex items-center gap-2 rounded-2xl border border-hairline bg-surface px-3.5 py-2 text-[13px] text-ink-faint">
                <Loader2 size={13} className="animate-spin" aria-hidden />
                Thinking
              </div>
            </div>
          )}

          {error && (
            <p role="alert" className="text-center text-[12px] text-warn-text">
              {error}
            </p>
          )}

          <div ref={endRef} />
        </div>
      </div>

      <div className="border-t border-hairline px-8 py-3">
        <div className="mx-auto flex max-w-2xl items-end gap-2">
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              // Enter sends, Shift+Enter breaks a line — the convention everywhere else.
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
            rows={1}
            placeholder="Ask about this meeting…"
            aria-label="Your question"
            className="max-h-32 flex-1 resize-none rounded-xl border border-hairline px-3 py-2
                       text-[14px] outline-none transition focus:border-hairline"
          />
          <button
            type="button"
            onClick={send}
            disabled={busy || draft.trim().length === 0}
            aria-label="Send"
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full
                       bg-accent text-accent-on transition hover:bg-accent
                       disabled:bg-hairline disabled:text-ink-faint"
          >
            <Send size={15} aria-hidden />
          </button>
        </div>
      </div>
    </div>
  );
}
