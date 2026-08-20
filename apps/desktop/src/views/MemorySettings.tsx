import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Brain, Trash2 } from "lucide-react";

import { api, ApiError, type MemoryItem } from "../lib/api";

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

  const load = useCallback(async () => {
    try {
      const r = await api.memories();
      setMemories(r.memories);
      setUsed(r.global_used);
      setCap(r.global_cap);
    } catch {
      /* Nothing remembered is the ordinary state and not worth an error banner. */
    }
  }, []);

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
    </section>
  );
}

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
