import { useCallback, useEffect, useState } from "react";
import { FileWarning, Loader2 } from "lucide-react";

import { api, ApiError, type VaultDivergence } from "../lib/api";

/**
 * Vault files you edited, and what to do about each.
 *
 * # Why this screen exists at all
 *
 * The vault has refused to overwrite an edited file for a while, correctly, and told nobody. A user
 * who annotated a meeting note in Obsidian got a mirror that quietly stopped updating and no way to
 * find out why — which is a worse version of the bug the refusal was fixing.
 *
 * # Why the file's content is shown
 *
 * The choice is about somebody's writing. Making it from a filename is making it blind, and the one
 * outcome nobody wants is discovering afterwards that "overwrite" discarded a paragraph they cared
 * about. So the file is read and shown before the three buttons.
 *
 * # Why there is no merge
 *
 * Three-way merge on prose without a common ancestor produces plausible text neither side wrote. For
 * a record of what was said in a meeting that is not an acceptable outcome, so the choice is
 * explicit and the third option — keep it as a note — is the one that loses nothing.
 */
export function VaultDivergences() {
  const [divergences, setDivergences] = useState<VaultDivergence[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setDivergences(await api.vaultDivergences());
    } catch {
      // No vault, or nothing diverged. Both are the ordinary state.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const resolve = async (
    divergence: VaultDivergence,
    resolution: "keep" | "overwrite" | "copy_to_note",
  ) => {
    setBusy(divergence.id);
    setError(null);
    try {
      const result = await api.resolveDivergence(divergence.id, resolution);
      setDone(result.message);
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not settle that.");
    } finally {
      setBusy(null);
    }
  };

  // Renders nothing when there is nothing to answer, which is almost always.
  if (divergences.length === 0 && !done) return null;

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <FileWarning size={13} className="text-ink-faint" aria-hidden />
        Files you edited
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        These vault files have changed since Notewise last wrote them, so it stopped writing to them
        rather than overwrite what you put there. Nothing has been lost — choose what should happen
        to each.
      </p>

      {error && (
        <div
          role="alert"
          className="mb-3 rounded-lg border border-warn-line bg-warn px-3 py-2 text-[12.5px] text-warn-text"
        >
          {error}
        </div>
      )}

      {done && divergences.length === 0 && (
        <p className="rounded-lg border border-hairline bg-overlay px-3 py-2 text-[12.5px] text-ink-muted">
          {done}
        </p>
      )}

      <ul className="space-y-3">
        {divergences.map((divergence) => (
          <li key={divergence.id} className="card overflow-hidden">
            <header className="border-b border-hairline px-3 py-2">
              <p className="truncate text-[12.5px] text-ink" title={divergence.path}>
                {divergence.file_name}
              </p>
              <p className="mt-0.5 truncate text-[11.5px] text-ink-faint">
                {divergence.meeting_title
                  ? `mirrors “${divergence.meeting_title}”`
                  : "the meeting it mirrored has been deleted"}
              </p>
            </header>

            <div className="px-3 py-2.5">
              <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
                What is in the file now
              </p>
              {divergence.current_content ? (
                <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded bg-overlay p-2
                                font-mono text-[11.5px] leading-snug text-ink-muted">
                  {divergence.current_content}
                </pre>
              ) : (
                <p className="text-[12px] leading-relaxed text-warn-text">
                  The file cannot be read — it may have moved, or something else may be holding it.
                  Notewise treats a file it cannot check as edited, because guessing “unchanged”
                  risks overwriting your work.
                </p>
              )}
            </div>

            <footer className="flex flex-wrap items-center gap-2 border-t border-hairline bg-overlay px-3 py-2">
              <button
                type="button"
                onClick={() => void resolve(divergence, "copy_to_note")}
                disabled={busy === divergence.id || !divergence.current_content}
                title="Save what you wrote as a note, then refresh the file"
                className="flex items-center gap-1 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                           text-accent-on transition hover:opacity-90 disabled:opacity-40"
              >
                {busy === divergence.id && (
                  <Loader2 size={11} className="animate-spin" aria-hidden />
                )}
                Keep as a note, then refresh
              </button>

              <button
                type="button"
                onClick={() => void resolve(divergence, "keep")}
                disabled={busy === divergence.id}
                title="Leave the file alone and stop mirroring this meeting to it"
                className="rounded-full border border-hairline px-2.5 py-1 text-[11.5px]
                           text-ink-muted transition hover:text-ink disabled:opacity-50"
              >
                Leave my file, stop mirroring
              </button>

              <button
                type="button"
                onClick={() => void resolve(divergence, "overwrite")}
                disabled={busy === divergence.id || !divergence.current_content}
                title="Replace the file with the current notes"
                className="rounded-full border border-hairline px-2.5 py-1 text-[11.5px]
                           text-warn-text transition hover:bg-warn disabled:opacity-40"
              >
                Discard my edits
              </button>

              <span className="flex-1" />
              <span className="text-[11px] text-ink-faint">
                {new Date(divergence.detected_at).toLocaleString([], {
                  day: "numeric",
                  month: "short",
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </span>
            </footer>
          </li>
        ))}
      </ul>
    </section>
  );
}
