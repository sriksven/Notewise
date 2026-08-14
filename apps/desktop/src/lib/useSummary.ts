import { useCallback, useEffect, useState } from "react";

import { api, ApiError, type Summary } from "./api";

export interface SummaryState {
  summary: Summary | null;
  loading: boolean;
  error: string | null;
  /** Re-read the stored summary. Does not run the model. */
  reload: () => Promise<void>;
}

/**
 * The stored summary for a meeting, held once for the whole window.
 *
 * Lifted out of the summary screen because the summary is no longer only a screen: the
 * intelligence panel shows the same decisions and action items alongside the transcript. Two
 * components fetching the same row independently would drift — one refreshed after summarizing
 * and the other not — and would double the requests to say so.
 *
 * Read, never generated. A summary is written once and looked at many times; re-running a model
 * on every selection would be slow and would give a different answer each time.
 */
export function useSummary(meetingId: string | null): SummaryState {
  const [summary, setSummary] = useState<Summary | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (id: string, signal: { cancelled: boolean }) => {
    setLoading(true);
    setError(null);
    try {
      const result = await api.summary(id);
      if (!signal.cancelled) setSummary(result.summary);
    } catch (e) {
      if (!signal.cancelled) {
        setError(e instanceof ApiError ? e.message : "Could not load the summary.");
      }
    } finally {
      if (!signal.cancelled) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!meetingId) {
      setSummary(null);
      setError(null);
      return;
    }

    // Cleared before the fetch, not after: leaving the previous meeting's decisions on screen
    // while another one loads is how a user ends up reading the wrong meeting's conclusions.
    setSummary(null);

    const signal = { cancelled: false };
    void load(meetingId, signal);
    return () => {
      signal.cancelled = true;
    };
  }, [meetingId, load]);

  const reload = useCallback(async () => {
    if (!meetingId) return;
    await load(meetingId, { cancelled: false });
  }, [meetingId, load]);

  return { summary, loading, error, reload };
}
