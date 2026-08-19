import { FollowUpDrafts } from "../components/FollowUpDrafts";
import { SummaryTemplatePicker } from "../components/SummaryTemplatePicker";
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
  /// Reload the summary without producing another one.
  ///
  /// Deliberately separate from `onSummarize`, which *runs* a default summarisation — passing that
  /// as a completion callback would fire a second, default-prompt summary straight after a
  /// templated one, and the default would win as the newest row.
  onReload: () => void;
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
  onReload,
}: Props) {
  if (!meetingId) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <p className="text-[13px] text-ink-faint">Select a meeting to see its summary.</p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto px-8 pb-28 pt-6">
      <div className="mx-auto max-w-2xl space-y-6">
        {error && (
          <div
            role="alert"
            className="rounded-lg border border-warn-line bg-warn px-3 py-2 text-[13px] text-warn-text"
          >
            {error}
          </div>
        )}

        {loading && <p className="text-[13px] text-ink-faint">Loading…</p>}

        {!loading && !summary && (
          <div className="flex flex-col items-start gap-3">
            <p className="text-[13px] text-ink-muted">
              {hasTranscript
                ? "This meeting has not been summarized yet."
                : "This meeting has no transcript to summarize."}
            </p>
            <button
              type="button"
              onClick={onSummarize}
              disabled={summarizing || !hasTranscript}
              className="flex items-center gap-1.5 rounded-lg border border-hairline px-3 py-2
                         text-[13px] text-ink transition hover:bg-overlay
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
            <p className="mt-4 text-[11px] text-ink-faint">
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

        <SummaryTemplatePicker
          meetingId={meetingId}
          hasTranscript={hasTranscript}
          onDone={onReload}
        />

        {/* Below the summary, because the engine drafts from the summary — offering it above would
            invite drafting from a transcript, which costs more tokens and reads worse. */}
        <FollowUpDrafts meetingId={meetingId} hasSource={summary !== null} />
      </div>
    </div>
  );
}
