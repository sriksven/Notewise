import { Loader2, Sparkles } from "lucide-react";

import { Markdown } from "../components/Markdown";
import type { Summary } from "../lib/api";

interface Props {
  meetingId: string | null;
  summary: Summary | null;
  loading: boolean;
  error: string | null;
  /** Summarizing an empty meeting produces confident nonsense, so it is not offered. */
  hasTranscript: boolean;
  summarizing: boolean;
  onSummarize: () => void;
}

/**
 * The summary as a document.
 *
 * The narrative only. Decisions and action items live in the intelligence panel, which is on
 * screen beside this — listing them here too put the same three decisions in front of the user
 * three times at once, since the model's own prose already enumerates them.
 *
 * Presentational. The summary is loaded once for the window and handed down, so this view and
 * the panel can never disagree about what was found.
 */
export function SummaryView({
  meetingId,
  summary,
  loading,
  error,
  hasTranscript,
  summarizing,
  onSummarize,
}: Props) {
  if (!meetingId) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <p className="text-[13px] text-neutral-400">Select a meeting to see its summary.</p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto px-8 pb-28 pt-6">
      <div className="mx-auto max-w-2xl space-y-6">
        {error && (
          <div
            role="alert"
            className="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-[13px] text-amber-900"
          >
            {error}
          </div>
        )}

        {loading && <p className="text-[13px] text-neutral-400">Loading…</p>}

        {!loading && !summary && (
          <div className="flex flex-col items-start gap-3">
            <p className="text-[13px] text-neutral-500">
              {hasTranscript
                ? "This meeting has not been summarized yet."
                : "This meeting has no transcript to summarize."}
            </p>
            <button
              type="button"
              onClick={onSummarize}
              disabled={summarizing || !hasTranscript}
              className="flex items-center gap-1.5 rounded-lg border border-hairline px-3 py-2
                         text-[13px] text-neutral-700 transition hover:bg-neutral-50
                         disabled:cursor-not-allowed disabled:opacity-50"
            >
              {summarizing ? (
                <>
                  <Loader2 size={13} className="animate-spin" aria-hidden />
                  Summarizing
                </>
              ) : (
                <>
                  <Sparkles size={13} aria-hidden />
                  Summarize
                </>
              )}
            </button>
          </div>
        )}

        {summary && (
          <section>
            <Markdown source={summary.text} />
            <p className="mt-4 text-[11px] text-neutral-400">
              Summarized with {summary.model} on{" "}
              {new Date(summary.created_at).toLocaleString([], {
                month: "short",
                day: "numeric",
                hour: "numeric",
                minute: "2-digit",
              })}
            </p>
          </section>
        )}
      </div>
    </div>
  );
}
