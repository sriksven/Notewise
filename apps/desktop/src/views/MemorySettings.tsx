import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Brain, Loader2, Sparkles, Trash2 } from "lucide-react";

import {
  api,
  ApiError,
  type ExtractionReport,
  type ExtractionStatus,
  type MemoryItem,
} from "../lib/api";

/**
 * What the app remembers about you.
 *
 * # Everything is on screen, always
 *
 * A memory is injected into the system prompt of future calls, so it shapes answers indefinitely and
 * invisibly. The only version of that which is defensible is one where every item is listed,
 * editable, and deletable — which is why this screen shows all of them rather than a summary, and
 * says which were written by hand and which were inferred.
 *
 * # Why the count is shown
 *
 * The limits are hard. A cap the user cannot see is a cap that arrives as a surprise refusal, so the
 * usage is displayed before it binds.
 */
export function MemorySettings() {
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [used, setUsed] = useState(0);
  const [cap, setCap] = useState(5);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [extraction, setExtraction] = useState<ExtractionStatus | null>(null);
  const [report, setReport] = useState<ExtractionReport | null>(null);
  const [running, setRunning] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await api.memories();
      setMemories(r.memories);
      setUsed(r.global_used);
      setCap(r.global_cap);
    } catch {
      /* Nothing remembered is the ordinary state and not worth an error banner. */
    }
    try {
      setExtraction(await api.extractionStatus());
    } catch {
      /* Same: a status that will not load is not worth a banner. */
    }
  }, []);

  async function toggleExtraction(enabled: boolean) {
    setError(null);
    // Optimistic, because the switch should move when it is clicked; the reload below corrects it.
    setExtraction((current) => (current ? { ...current, enabled } : current));
    try {
      await api.setExtractionEnabled(enabled);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not change that");
    }
    await load();
  }

  async function runNow() {
    setRunning(true);
    setError(null);
    try {
      setReport(await api.runExtraction());
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not read your meetings");
    } finally {
      setRunning(false);
    }
  }

  useEffect(() => {
    void load();
  }, [load]);

  async function add() {
    setBusy(true);
    setError(null);
    try {
      await api.createMemory(draft, "global");
      setDraft("");
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not save that");
    } finally {
      setBusy(false);
    }
  }

  async function remove(id: string) {
    setError(null);
    try {
      await api.deleteMemory(id);
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not delete that");
    }
  }

  async function edit(id: string, text: string) {
    setError(null);
    try {
      await api.updateMemory(id, text);
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not save that");
    }
  }

  return (
    <section className="mt-8 border-t border-hairline pt-6">
      <h2 className="mb-1 flex items-center gap-2 text-[13px] font-semibold text-ink">
        <Brain className="h-3.5 w-3.5" /> What it remembers about you
      </h2>
      <p className="mb-3 max-w-2xl text-[12.5px] leading-relaxed text-ink-muted">
        These are added to the instructions for summaries and answers, so they shape what you get
        back. Everything is listed here and nothing is hidden. Facts about other people are never
        stored — a note about a colleague would follow them into every future answer.
      </p>

      {error && (
        <div className="mb-3 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-[12.5px] text-amber-200">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      <div className="flex items-center gap-2">
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && draft.trim() !== "") {
              e.preventDefault();
              void add();
            }
          }}
          placeholder="e.g. I prefer short summaries with bullet points"
          aria-label="New memory"
          className="flex-1 rounded-md border border-hairline bg-transparent px-3 py-2 text-[13px] text-ink outline-none focus:border-accent/40"
        />
        <button
          type="button"
          disabled={busy || draft.trim() === "" || used >= cap}
          onClick={() => void add()}
          className="btn-ghost px-3 py-2 text-[12.5px] disabled:opacity-50"
        >
          Remember
        </button>
      </div>

      <p className="mt-1.5 text-[11.5px] text-ink-faint">
        {used} of {cap} used
        {used >= cap && " — delete one to make room"}
      </p>

      <ul className="mt-4 space-y-2">
        {memories.map((m) => (
          <li key={m.id} className="flex items-start gap-3 rounded-lg border border-hairline p-3">
            <div className="min-w-0 flex-1">
              <EditableMemory text={m.text} onSave={(next) => edit(m.id, next)} />
              <p className="mt-1 text-[11px] text-ink-faint">
                {m.origin === "manual" ? "you added this" : "inferred from a meeting"}
                {m.scope === "project" && " · this project only"}
              </p>
            </div>
            <button
              type="button"
              onClick={() => void remove(m.id)}
              aria-label="Forget this"
              className="btn-ghost shrink-0 px-2 py-1 text-[12px]"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </li>
        ))}
      </ul>

      {memories.length === 0 && (
        <p className="mt-3 text-[12.5px] text-ink-faint">
          Nothing yet. Anything you add here applies to every meeting.
        </p>
      )}

      {/* Automatic extraction, off until asked for. A feature that reads every transcript to build a
          durable profile belongs in the same category as voiceprints and acoustic separation, both of
          which ship off — and the manual list above works whether or not this is ever turned on. */}
      {extraction && (
        <div className="mt-5 rounded-lg border border-hairline p-3">
          <label className="flex cursor-pointer items-start gap-2">
            <input
              type="checkbox"
              checked={extraction.enabled}
              onChange={(e) => void toggleExtraction(e.target.checked)}
              className="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
            />
            <span className="min-w-0">
              <span className="flex items-center gap-1.5 text-[12.5px] text-ink">
                <Sparkles className="h-3 w-3 text-ink-faint" />
                Learn from my meetings
              </span>
              <span className="mt-0.5 block text-[11.5px] leading-relaxed text-ink-faint">
                Reads meetings you have already recorded and notes durable facts about you — your
                role, your projects, how you like things written. Never facts about other people:
                anything that describes somebody else is discarded, and everything kept appears in
                the list above where you can change or delete it.
              </span>
            </span>
          </label>

          <div className="mt-2.5 flex items-center gap-2 border-t border-hairline pt-2.5">
            <button
              type="button"
              onClick={() => void runNow()}
              disabled={running}
              className="btn-ghost flex items-center gap-1.5 px-2.5 py-1 text-[12px] disabled:opacity-50"
            >
              {running && <Loader2 className="h-3 w-3 animate-spin" />}
              Read my meetings now
            </button>
            <p className="min-w-0 flex-1 text-[11.5px] text-ink-faint">
              {extraction.unprocessed === 0
                ? "Nothing new to read."
                : `${extraction.unprocessed} meeting${
                    extraction.unprocessed === 1 ? "" : "s"
                  } not read yet.`}
              {extraction.blocked_by && extraction.enabled && ` Automatically: ${extraction.blocked_by}.`}
            </p>
          </div>

          {report && <RunReport report={report} />}
        </div>
      )}
    </section>
  );
}

/**
 * What the last run decided, candidate by candidate.
 *
 * Shown rather than summarised because the two questions this feature generates are "why does it not
 * remember that" and "why does it think that", and a count answers neither. A refusal is more
 * interesting than an acceptance here — it is the only visible evidence that the third-party rule and
 * the secret rule are doing anything.
 */
function RunReport({ report }: { report: ExtractionReport }) {
  const label: Record<ExtractionDecisionVerdict, string> = {
    kept: "remembered",
    duplicate: "already known",
    third_party: "about someone else — discarded",
    secret: "looked like a secret — discarded",
    unusable: "not usable",
  };

  return (
    <div className="mt-2.5 border-t border-hairline pt-2.5">
      {report.skipped ? (
        <p className="text-[11.5px] text-ink-faint">{report.skipped}</p>
      ) : (
        <p className="text-[11.5px] text-ink-faint">
          Read {report.meetings_read} meeting{report.meetings_read === 1 ? "" : "s"}, considered{" "}
          {report.proposed}, remembered {report.kept}.
          {report.proposed === 0 && " Most meetings contain nothing durable, which is normal."}
        </p>
      )}

      {report.decisions.length > 0 && (
        <ul className="mt-1.5 space-y-1">
          {report.decisions.map((decision, index) => (
            <li key={index} className="text-[11.5px] leading-relaxed">
              <span className={decision.verdict === "kept" ? "text-ink" : "text-ink-faint"}>
                {decision.text}
              </span>
              <span className="text-ink-faint"> — {label[decision.verdict]}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

type ExtractionDecisionVerdict = ExtractionReport["decisions"][number]["verdict"];

/**
 * One memory, correctable in place.
 *
 * Editing rather than delete-and-retype: a memory that is nearly right is the common case, and
 * retyping it invites deleting the wrong one.
 */
function EditableMemory({
  text,
  onSave,
}: {
  text: string;
  onSave: (next: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(text);

  async function commit() {
    const next = draft.trim();
    if (next === "" || next === text) {
      setDraft(text);
      setEditing(false);
      return;
    }
    await onSave(next);
    setEditing(false);
  }

  if (!editing) {
    return (
      <p
        className="cursor-text text-[13px] leading-relaxed text-ink"
        title="Double-click to edit"
        onDoubleClick={() => setEditing(true)}
      >
        {text}
      </p>
    );
  }

  return (
    <input
      autoFocus
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => void commit()}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          void commit();
        }
        if (e.key === "Escape") {
          setDraft(text);
          setEditing(false);
        }
      }}
      aria-label="Edit memory"
      className="w-full rounded-md border border-accent/40 bg-transparent px-2 py-1 text-[13px] text-ink outline-none"
    />
  );
}
