import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Check, Loader2, Sparkles } from "lucide-react";

import { api, ApiError, type IndexStatus } from "../lib/api";

/** How often to re-read a running build. */
const POLL_MS = 1_500;

/**
 * Semantic search — what it is, whether it is on, and how to turn it on.
 *
 * Presented as a *search quality* setting rather than an AI one, because that is what it
 * changes. It is also the only place the app can explain why a question about "pricing" does
 * not find a meeting that said "cost structure", and what to do about it.
 *
 * The privacy line matters and is stated up front: embedding runs on this machine through
 * Ollama, never through the configured chat backend. A user who picked a hosted provider for
 * summaries has not agreed to their whole history being uploaded to build an index.
 */
export function SearchIndexSettings() {
  const [status, setStatus] = useState<IndexStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setStatus(await api.indexStatus());
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not read the index.");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Poll only while a build is going; an idle settings screen does no work.
  useEffect(() => {
    if (status?.state !== "running") return;
    const id = setInterval(() => void load(), POLL_MS);
    return () => clearInterval(id);
  }, [status?.state, load]);

  const build = async () => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await api.buildIndex());
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not start indexing.");
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.clearIndex();
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not clear the index.");
    } finally {
      setBusy(false);
    }
  };

  const running = status?.state === "running";
  const on = (status?.chunks ?? 0) > 0;

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Sparkles size={14} className="text-ink-faint" aria-hidden />
        Search by meaning
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Without this, search matches words: asking about “pricing” will not find a meeting that
        only ever said “cost structure”. Building an index fixes that for questions, the Ask
        tabs, and the agent. Word search keeps working either way.
      </p>

      <div className="card overflow-hidden">
        <div className="flex items-start gap-3 px-4 py-3">
          <span
            className={`mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full ${
              on ? "bg-ok text-ok-text" : "bg-overlay text-ink-faint"
            }`}
          >
            {running ? (
              <Loader2 size={11} className="animate-spin" aria-hidden />
            ) : on ? (
              <Check size={11} strokeWidth={3} aria-hidden />
            ) : (
              <span className="h-1.5 w-1.5 rounded-full bg-current" aria-hidden />
            )}
          </span>

          <div className="min-w-0 flex-1">
            <p className="text-[13px] font-medium text-ink">
              {running
                ? `Indexing — ${status?.done ?? 0} of ${status?.total ?? 0}`
                : on
                  ? "On"
                  : "Not built yet"}
            </p>
            <p className="mt-0.5 text-[12px] leading-relaxed text-ink-muted">
              {status
                ? on
                  ? `${status.chunks.toLocaleString()} passages indexed with ${status.model}, on this machine.`
                  : `Uses ${status.model} through Ollama. Nothing is uploaded — embedding never goes to your chat provider.`
                : "Reading…"}
            </p>
          </div>

          <div className="flex shrink-0 gap-1">
            <button
              type="button"
              onClick={() => void build()}
              disabled={busy || running || !status?.available}
              className="btn-quiet py-1.5"
            >
              {on ? "Update" : "Build index"}
            </button>
            {on && !running && (
              <button
                type="button"
                onClick={() => void clear()}
                disabled={busy}
                className="rounded-lg border border-hairline px-2.5 py-1.5 text-[13px]
                           text-ink-muted transition hover:bg-overlay hover:text-ink
                           disabled:opacity-50"
              >
                Clear
              </button>
            )}
          </div>
        </div>

        {status && !status.available && (
          <div className="flex items-start gap-2 border-t border-warn-line bg-warn px-4 py-2.5 text-[12px] leading-relaxed text-warn-text">
            <AlertTriangle size={13} className="mt-0.5 shrink-0" aria-hidden />
            <span>
              Ollama is not running, or <code>{status.model}</code> has not been pulled. Run{" "}
              <code>ollama pull {status.model}</code> and try again. Search still works by word
              in the meantime.
            </span>
          </div>
        )}

        {/* Vectors from a model the user has stopped using can never be compared against the
            current one, so they are dead weight in every query. Say so rather than quietly
            carrying them. */}
        {status && status.stale_from_other_models > 0 && (
          <p className="border-t border-hairline bg-overlay px-4 py-2 text-[12px] text-ink-muted">
            {status.stale_from_other_models.toLocaleString()} passages were indexed with a
            different model and cannot be used. Clearing removes them.
          </p>
        )}

        {status?.error && (
          <p className="border-t border-warn-line bg-warn px-4 py-2 text-[12px] leading-relaxed text-warn-text">
            {status.error}
          </p>
        )}
      </div>

      <p className="mt-2 text-[11px] leading-snug text-ink-faint">
        Once built, this keeps itself up to date — a note you write becomes answerable a few
        seconds after you stop typing, with nothing to press. Updating by hand only matters after
        restoring a backup, or if a pass failed.
      </p>

      {error && (
        <p role="alert" className="mt-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </section>
  );
}
