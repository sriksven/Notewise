import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Clock, Pause, Play, Plus, Trash2 } from "lucide-react";

import { api, ApiError, type Job, type JobRun } from "../lib/api";

/**
 * Scheduled jobs.
 *
 * # Why the preview is not optional
 *
 * A cron expression is easy to get wrong and the mistake is invisible: the job simply never runs,
 * and nobody notices until they wonder where their Monday digest went. So the next few fire times are
 * shown before anything is saved, and the engine refuses an expression it cannot parse.
 *
 * # Why the run history is here rather than hidden
 *
 * Nobody was present when a scheduled run happened. "It failed on Tuesday and I need to know why" is
 * the ordinary case, so the trace and the skip reasons are on the same screen as the schedule that
 * produced them.
 */
export function JobsView() {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [runs, setRuns] = useState<Record<string, JobRun[]>>({});
  const [open, setOpen] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [name, setName] = useState("");
  const [prompt, setPrompt] = useState("");
  const [cron, setCron] = useState("0 9 * * MON");
  const [preview, setPreview] = useState<string[] | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setJobs(await api.jobs());
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not load jobs");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Check the expression as it is typed. The engine is the authority on what parses, so this asks it
  // rather than reimplementing cron in the browser and disagreeing.
  useEffect(() => {
    let cancelled = false;
    if (cron.trim() === "") {
      setPreview(null);
      setPreviewError(null);
      return;
    }

    const timer = setTimeout(() => {
      void api
        .previewSchedule(cron)
        .then((r) => {
          if (cancelled) return;
          setPreview(r.next);
          setPreviewError(null);
        })
        .catch((e) => {
          if (cancelled) return;
          setPreview(null);
          setPreviewError(e instanceof ApiError ? e.message : "that schedule will not run");
        });
    }, 300);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [cron]);

  async function create() {
    setBusy(true);
    setError(null);
    try {
      await api.createJob({ name, prompt, cron });
      setName("");
      setPrompt("");
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "could not create that job");
    } finally {
      setBusy(false);
    }
  }

  async function showRuns(id: string) {
    if (open === id) {
      setOpen(null);
      return;
    }
    setOpen(id);
    try {
      setRuns((current) => ({ ...current, [id]: [] }));
      const list = await api.jobRuns(id);
      setRuns((current) => ({ ...current, [id]: list }));
    } catch {
      /* An unreadable history should not take the schedule list down with it. */
    }
  }

  async function act(fn: () => Promise<unknown>) {
    setError(null);
    try {
      await fn();
      await load();
      if (open) setRuns((c) => ({ ...c, [open]: [] }));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "that did not work");
    }
  }

  return (
    <div className="flex-1 overflow-y-auto px-8 pb-16 pt-6">
      <div className="mx-auto max-w-2xl">
        <h1 className="text-[17px] font-semibold tracking-tight text-ink">Scheduled jobs</h1>
        <p className="mt-1 text-[13px] leading-relaxed text-ink-muted">
          A job gives the agent a task on a schedule. It can search, read, and write a note — the same
          things it can do when you ask it directly. It cannot act in anything outside this app while
          nobody is watching.
        </p>

        {error && (
          <div className="mt-4 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-[12.5px] text-amber-200">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            <span>{error}</span>
          </div>
        )}

        <section className="mt-6 rounded-lg border border-hairline p-4">
          <h2 className="mb-3 flex items-center gap-2 text-[13px] font-semibold text-ink">
            <Plus className="h-3.5 w-3.5" /> New job
          </h2>

          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Name, e.g. Weekly digest"
            aria-label="Job name"
            className="mb-2 w-full rounded-md border border-hairline bg-transparent px-3 py-2 text-[13px] text-ink outline-none focus:border-accent/40"
          />
          <textarea
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            rows={3}
            placeholder="What should it do? e.g. Summarise the decisions from this week's meetings and write them up."
            aria-label="Job instructions"
            className="mb-2 w-full resize-none rounded-md border border-hairline bg-transparent px-3 py-2 text-[13px] text-ink outline-none focus:border-accent/40"
          />
          <input
            value={cron}
            onChange={(e) => setCron(e.target.value)}
            placeholder="0 9 * * MON"
            aria-label="Schedule"
            className="w-full rounded-md border border-hairline bg-transparent px-3 py-2 font-mono text-[12.5px] text-ink outline-none focus:border-accent/40"
          />

          {previewError && (
            <p className="mt-2 text-[12px] text-amber-200">{previewError}</p>
          )}
          {preview && (
            <div className="mt-2 text-[12px] text-ink-faint">
              Next:{" "}
              {preview.slice(0, 3).map((t, i) => (
                <span key={t}>
                  {i > 0 && ", "}
                  {new Date(t).toLocaleString([], {
                    weekday: "short",
                    hour: "numeric",
                    minute: "2-digit",
                  })}
                </span>
              ))}
            </div>
          )}

          <button
            type="button"
            disabled={busy || !name.trim() || !prompt.trim() || preview === null}
            onClick={() => void create()}
            className="btn-primary mt-3 px-3 py-1.5 text-[12.5px] disabled:opacity-50"
          >
            {busy ? "Creating…" : "Create job"}
          </button>
        </section>

        <ul className="mt-6 space-y-3">
          {jobs.map((job) => (
            <li key={job.id} className="rounded-lg border border-hairline p-4">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <p className="text-[13.5px] font-medium text-ink">{job.name}</p>
                  <p className="mt-0.5 font-mono text-[11.5px] text-ink-faint">
                    {job.cron} · {job.timezone}
                  </p>
                  <p className="mt-1 text-[12.5px] leading-relaxed text-ink-muted">{job.prompt}</p>
                  <p className="mt-1.5 flex items-center gap-1.5 text-[11.5px] text-ink-faint">
                    <Clock className="h-3 w-3" />
                    {job.enabled
                      ? job.next_fire
                        ? `next ${new Date(job.next_fire).toLocaleString()}`
                        : "that schedule no longer parses"
                      : "paused"}
                  </p>
                </div>

                <div className="flex shrink-0 items-center gap-1">
                  <button
                    type="button"
                    onClick={() => void act(() => api.runJob(job.id))}
                    title="Run it now"
                    className="btn-ghost px-2 py-1 text-[12px]"
                  >
                    Run now
                  </button>
                  <button
                    type="button"
                    onClick={() => void act(() => api.setJobEnabled(job.id, !job.enabled))}
                    aria-label={job.enabled ? "Pause job" : "Resume job"}
                    className="btn-ghost px-2 py-1 text-[12px]"
                  >
                    {job.enabled ? (
                      <Pause className="h-3.5 w-3.5" />
                    ) : (
                      <Play className="h-3.5 w-3.5" />
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => void act(() => api.deleteJob(job.id))}
                    aria-label="Delete job"
                    className="btn-ghost px-2 py-1 text-[12px]"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>

              <button
                type="button"
                onClick={() => void showRuns(job.id)}
                className="mt-2 text-[12px] text-accent-soft hover:underline"
              >
                {open === job.id ? "Hide history" : "History"}
              </button>

              {open === job.id && (
                <ul className="mt-2 space-y-1.5 border-t border-hairline pt-2">
                  {(runs[job.id] ?? []).length === 0 && (
                    <li className="text-[12px] text-ink-faint">It has not run yet.</li>
                  )}
                  {(runs[job.id] ?? []).map((run) => (
                    <li key={run.id} className="text-[12px] text-ink-muted">
                      <span className="font-mono">
                        {new Date(run.started_at).toLocaleString()}
                      </span>{" "}
                      — {run.status}
                      {/* The reason is the point of recording a skip at all. */}
                      {run.error && <span className="text-ink-faint"> · {run.error}</span>}
                      {run.trace && (
                        <span className="text-ink-faint"> · {run.trace.length} steps</span>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>

        {jobs.length === 0 && (
          <p className="mt-6 text-[13px] text-ink-faint">
            Nothing is scheduled. A weekly digest of decisions is a good first one.
          </p>
        )}
      </div>
    </div>
  );
}
