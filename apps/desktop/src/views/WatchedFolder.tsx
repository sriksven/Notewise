import { useState } from "react";
import { AlertTriangle, FolderSearch, Loader2 } from "lucide-react";

import { api, ApiError, type AvailableConnector } from "../lib/api";

interface Props {
  connector: AvailableConnector | undefined;
  onChanged: () => void | Promise<void>;
}

/**
 * A folder whose documents become searchable.
 *
 * # Why this is a separate card rather than one of the sink rows
 *
 * Every other connector in that list is somewhere Notewise *writes*. This is somewhere it reads, and
 * the distinction is the one a user cares about most: a folder connected here is never modified, and
 * the row that says "Folder" next to the vault means the opposite.
 *
 * # What it does not do
 *
 * It does not open a picker. A browser cannot hand a path to an engine — `showDirectoryPicker` gives
 * a handle scoped to the page, not a path — and the engine is what has to read the files. So the path
 * is typed or pasted, and the engine refuses one that is not a folder rather than accepting it and
 * reading nothing.
 */
export function WatchedFolder({ connector, onChanged }: Props) {
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!connector) return null;

  const connect = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.connectConnector(connector.id, path.trim());
      setPath("");
      await onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not watch that folder.");
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.disconnectConnector(connector.id);
      await onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not stop watching.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="mt-7">
      <h2 className="mb-1 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
        <FolderSearch size={13} className="text-ink-faint" aria-hidden />
        Documents to read
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Text and Markdown files in a folder you choose become searchable alongside your meetings, so
        an answer can cite a spec you wrote elsewhere. Read only — nothing in this folder is ever
        changed, moved, or written to.
      </p>

      <div className="card p-3">
        {connector.connected ? (
          <div className="flex items-center gap-2">
            <p className="min-w-0 flex-1 text-[12.5px] text-ink">
              Watching a folder.
              <span className="block text-[11.5px] text-ink-faint">
                Checked every ten minutes. A file that disappears keeps its place in search rather
                than becoming uncitable.
              </span>
            </p>
            <button
              type="button"
              onClick={() => void disconnect()}
              disabled={busy}
              className="btn-ghost shrink-0 px-2.5 py-1 text-[11.5px] disabled:opacity-50"
            >
              Stop watching
            </button>
          </div>
        ) : (
          <div className="space-y-2">
            <input
              value={path}
              onChange={(event) => setPath(event.target.value)}
              placeholder="/Users/you/Documents/Specs"
              spellCheck={false}
              className="w-full rounded border border-hairline bg-transparent px-2 py-1 font-mono
                         text-[12px] text-ink placeholder:text-ink-faint"
            />
            <p className="text-[11px] leading-relaxed text-ink-faint">
              A full path. Notewise reads at most a few hundred files, skips anything large or
              binary, and never follows the folder above the one you name.
            </p>
            <button
              type="button"
              onClick={() => void connect()}
              disabled={busy || !path.trim()}
              className="flex items-center gap-1.5 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                         text-accent-on transition hover:opacity-90 disabled:opacity-50"
            >
              {busy && <Loader2 size={11} className="animate-spin" aria-hidden />}
              Watch this folder
            </button>
          </div>
        )}

        {error && (
          <p className="mt-2 flex items-start gap-1.5 text-[11.5px] leading-relaxed text-warn-text">
            <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
            {error}
          </p>
        )}
      </div>
    </section>
  );
}
