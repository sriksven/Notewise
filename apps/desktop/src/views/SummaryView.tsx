import { useEffect, useState } from "react";
import { CircleCheck, Gavel, Loader2, Sparkles, User } from "lucide-react";

import { api, ApiError, type Summary } from "../lib/api";

interface Props {
  meetingId: string | null;
  meetingTitle: string | null;
  /** Summarizing an empty meeting produces confident nonsense, so it is not offered. */
  hasTranscript: boolean;
}

/**
 * The summary, its decisions and its action items.
 *
 * A view rather than a toast line. Summarization already worked, but its output only ever
 * appeared as one sentence of transient notice text — generated, stored, and then effectively
 * thrown away. Decisions and action items were counted and never shown at all.
 *
 * Loaded rather than generated on open: a summary is written once and read many times, and
 * re-running a model on every visit would be slow and would give a different answer each time.
 */
export function SummaryView({ meetingId, meetingTitle, hasTranscript }: Props) {
  const [summary, setSummary] = useState<Summary | null>(null);
  const [loading, setLoading] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!meetingId) {
      setSummary(null);
      return;
    }

    let cancelled = false;
    setLoading(true);
    api
      .summary(meetingId)
      .then((result) => {
        if (!cancelled) setSummary(result.summary);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : "Could not load.");
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  const generate = async () => {
    if (!meetingId) return;
    setGenerating(true);
    setError(null);
    try {
      await api.summarize(meetingId);
      const result = await api.summary(meetingId);
      setSummary(result.summary);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not summarize.");
    } finally {
      setGenerating(false);
    }
  };

  if (!meetingId) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <p className="text-[13px] text-neutral-400">Select a meeting to see its summary.</p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
      <div className="mx-auto max-w-2xl space-y-6">
        <header className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h1 className="truncate text-[20px] font-semibold tracking-tight">
              {meetingTitle ?? "Meeting"}
            </h1>
            {summary && (
              <p className="mt-0.5 text-[12px] text-neutral-500">
                Summarized with {summary.model}
              </p>
            )}
          </div>

          <button
            type="button"
            onClick={generate}
            disabled={generating || !hasTranscript}
            title={
              hasTranscript
                ? "Runs on the configured backend"
                : "Needs a transcript first"
            }
            className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline px-3 py-1.5
                       text-[12px] text-neutral-700 transition hover:bg-neutral-50
                       disabled:cursor-not-allowed disabled:opacity-50"
          >
            {generating ? (
              <>
                <Loader2 size={13} className="animate-spin" aria-hidden />
                Summarizing
              </>
            ) : (
              <>
                <Sparkles size={13} aria-hidden />
                {summary ? "Regenerate" : "Summarize"}
              </>
            )}
          </button>
        </header>

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
          <p className="text-[13px] text-neutral-500">
            {hasTranscript
              ? "Not summarized yet."
              : "This meeting has no transcript to summarize."}
          </p>
        )}

        {summary && (
          <>
            <section>
              <h2 className="mb-2 text-[13px] font-semibold text-neutral-900">Summary</h2>
              {/* `whitespace-pre-wrap` because models emit paragraphs and bullet lists as
                  newlines; collapsing them would turn a structured summary into a wall. */}
              <p className="whitespace-pre-wrap text-[14px] leading-relaxed text-neutral-700">
                {summary.text}
              </p>
            </section>

            <section>
              <h2 className="mb-2 flex items-center gap-1.5 text-[13px] font-semibold text-neutral-900">
                <Gavel size={14} className="text-neutral-400" aria-hidden />
                Decisions
              </h2>
              {summary.decisions.length === 0 ? (
                // Stated rather than hidden: "no decisions were reached" is itself a finding
                // about a meeting, and an absent section reads as a missing feature.
                <p className="text-[13px] text-neutral-400">
                  No decisions were identified.
                </p>
              ) : (
                <ul className="space-y-2">
                  {summary.decisions.map((decision) => (
                    <li
                      key={decision.id}
                      className="rounded-lg border border-hairline bg-rail px-3 py-2"
                    >
                      <p className="text-[13px] text-neutral-800">{decision.text}</p>
                      {decision.reasoning && (
                        <p className="mt-1 text-[12px] text-neutral-500">
                          {decision.reasoning}
                        </p>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </section>

            <section>
              <h2 className="mb-2 flex items-center gap-1.5 text-[13px] font-semibold text-neutral-900">
                <CircleCheck size={14} className="text-neutral-400" aria-hidden />
                Action items
              </h2>
              {summary.action_items.length === 0 ? (
                <p className="text-[13px] text-neutral-400">
                  No action items were identified.
                </p>
              ) : (
                <ul className="space-y-1.5">
                  {summary.action_items.map((item) => (
                    <li key={item.id} className="flex items-start gap-2 text-[13px]">
                      <span className="mt-[3px] h-1.5 w-1.5 shrink-0 rounded-full bg-neutral-300" />
                      <span className="min-w-0">
                        <span className="text-neutral-800">{item.text}</span>{" "}
                        {/* An unassigned item is shown as unassigned rather than left blank:
                            a blank owner reads as a rendering bug, and the whole point is that
                            nobody picked it up. */}
                        <span
                          className={`inline-flex items-center gap-1 text-[12px] ${
                            item.owner ? "text-neutral-500" : "text-amber-700"
                          }`}
                        >
                          <User size={11} aria-hidden />
                          {item.owner ?? "unassigned"}
                        </span>
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  );
}
