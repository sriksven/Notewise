import { useCallback, useEffect, useState } from "react";
import { Plus, X } from "lucide-react";

import { api, ApiError, type Decision } from "../lib/api";

interface Props {
  meetingId: string;
  /** Bumped by the parent after a summary run, so newly extracted decisions appear. */
  refreshToken?: number;
}

/**
 * What the room decided.
 *
 * # Read from the meeting, not the summary
 *
 * `IntelPanel` used to render `summary.decisions`, which is `SummaryRepository::decisions(summary_id)`
 * — the decisions belonging to *one* summary. Summarising appends rather than updates and the newest
 * row wins, so running a second template hid every decision the first run found. They were still
 * there; `decisions_for_meeting` exists for exactly this and its doc says so: "every decision made in
 * a meeting, whichever summary first surfaced it."
 *
 * This is the same correction already made for action items, in the same panel, for the same reason.
 * Decisions were the half left behind — invisible until now because nothing could add one by hand
 * either, so there was never a decision that no summary had proposed.
 *
 * # Why removing one matters
 *
 * A wrong decision is worse than a missing one: it reads as a record of what a group agreed. So the
 * remove control stays, and adding one is offered beside it — a decision the model missed is at
 * least as common as one it invented.
 */
export function Decisions({ meetingId, refreshToken = 0 }: Props) {
  const [decisions, setDecisions] = useState<Decision[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setDecisions(await api.decisions(meetingId));
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load decisions.");
    } finally {
      setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
  }, [load, refreshToken]);

  async function remove(decision: Decision) {
    const before = decisions;
    // Optimistic: the row should go when it is clicked. A decision that looks deleted and is not
    // would reappear later with no explanation.
    setDecisions((current) => current.filter((d) => d.id !== decision.id));
    try {
      await api.deleteDecision(decision.id);
      setError(null);
    } catch (e) {
      // Already gone is the outcome that was wanted.
      if (e instanceof ApiError && e.status === 404) return;
      setDecisions(before);
      setError(e instanceof ApiError ? e.message : "Could not remove that.");
    }
  }

  async function add() {
    const text = draft.trim();
    if (!text) return;

    setDraft("");
    setAdding(false);
    try {
      const created = await api.createDecision(meetingId, { text });
      setDecisions((current) => [...current, created]);
      setError(null);
    } catch (e) {
      // Put the text back rather than discarding it, as the action-item form does.
      setDraft(text);
      setAdding(true);
      setError(e instanceof ApiError ? e.message : "Could not add that.");
    }
  }

  if (loading && decisions.length === 0) {
    return <p className="text-[12px] leading-relaxed text-ink-faint">Loading…</p>;
  }

  return (
    <div>
      {error && (
        <p role="alert" className="mb-2 text-[11.5px] text-danger-text">
          {error}
        </p>
      )}

      {decisions.length === 0 ? (
        // Stated rather than hidden: "no decisions were reached" is itself a finding about a
        // meeting, and an absent section reads as a missing feature.
        <p className="text-[12px] leading-relaxed text-ink-faint">No decisions recorded.</p>
      ) : (
        <ul className="space-y-2">
          {decisions.map((decision) => (
            <li
              key={decision.id}
              className="group flex items-start gap-2 rounded-lg border border-hairline
                         bg-surface p-2.5"
            >
              <div className="min-w-0 flex-1">
                <p className="text-[12.5px] leading-snug text-ink">{decision.text}</p>
                {decision.reasoning && (
                  <p className="mt-1 text-[11px] leading-snug text-ink-muted">
                    {decision.reasoning}
                  </p>
                )}
              </div>

              <button
                type="button"
                onClick={() => void remove(decision)}
                aria-label={`Forget: ${decision.text}`}
                title="This was not a decision"
                className="shrink-0 rounded p-0.5 text-ink-faint opacity-0 transition
                           hover:text-warn-text group-hover:opacity-100 focus-visible:opacity-100"
              >
                <X size={11} aria-hidden />
              </button>
            </li>
          ))}
        </ul>
      )}

      {adding ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void add();
          }}
          className="mt-2"
        >
          <input
            autoFocus
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => {
              if (!draft.trim()) setAdding(false);
            }}
            placeholder="What was decided"
            aria-label="New decision"
            className="w-full rounded border border-hairline bg-surface px-2 py-1 text-[12.5px]
                       text-ink placeholder:text-ink-faint"
          />
        </form>
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="mt-2 flex items-center gap-1 text-[11.5px] text-ink-faint transition
                     hover:text-ink"
        >
          <Plus size={11} aria-hidden />
          Add a decision
        </button>
      )}
    </div>
  );
}
