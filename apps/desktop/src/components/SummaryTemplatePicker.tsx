import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Loader2, RefreshCw, Wand2 } from "lucide-react";

import { api, ApiError, type SummaryTemplate } from "../lib/api";

interface Props {
  meetingId: string;
  /** Summarising an empty meeting produces confident nonsense, so it is not offered. */
  hasTranscript: boolean;
  /** Reload the summary after a run. */
  onDone: () => void;
}

/**
 * Summarise this meeting a different way.
 *
 * # Why regenerating adds rather than replaces
 *
 * `SummaryRepository` has no `update` — summarising appends, and the newest row wins. So trying
 * another template is free: the previous summary is still there, and the decisions and action items
 * it produced survive because since v6 they carry their own `meeting_id` and only reference the
 * summary as provenance.
 *
 * That is worth saying on screen. Someone who has edited action items will not press a button
 * labelled "regenerate" unless they know it cannot take them away.
 */
export function SummaryTemplatePicker({ meetingId, hasTranscript, onDone }: Props) {
  const [templates, setTemplates] = useState<SummaryTemplate[]>([]);
  const [running, setRunning] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setTemplates(await api.summaryTemplates());
    } catch {
      // A template list that will not load should not take the summary down with it.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function run(template: SummaryTemplate) {
    setRunning(template.id);
    setError(null);
    try {
      await api.summarizeWithTemplate(meetingId, template.id);
      onDone();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not summarise that way");
    } finally {
      setRunning(null);
    }
  }

  if (!hasTranscript || templates.length === 0) return null;

  return (
    <section className="mt-6 border-t border-hairline pt-5">
      <h3 className="mb-1 flex items-center gap-2 text-[13px] font-semibold text-ink">
        <Wand2 className="h-3.5 w-3.5" /> Summarise it differently
      </h3>
      <p className="mb-3 max-w-2xl text-[12.5px] leading-relaxed text-ink-muted">
        Each of these is a different prompt. Running one adds a new summary — the one you have now,
        and any action items you have edited, stay exactly as they are.
      </p>

      {error && (
        <div className="mb-3 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-[12.5px] text-amber-200">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      <div className="flex flex-wrap gap-2">
        {templates.map((t) => (
          <button
            key={t.id}
            type="button"
            disabled={running !== null}
            onClick={() => void run(t)}
            title={t.prompt}
            className="btn-ghost px-3 py-1.5 text-[12px] disabled:opacity-50"
          >
            {running === t.id ? (
              <Loader2 className="mr-1.5 inline h-3 w-3 animate-spin" />
            ) : (
              <RefreshCw className="mr-1.5 inline h-3 w-3" />
            )}
            {t.name}
          </button>
        ))}
      </div>
    </section>
  );
}
