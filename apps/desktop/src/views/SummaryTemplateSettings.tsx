import { useCallback, useEffect, useState } from "react";
import { Check, Loader2, Pencil, Plus, Trash2, Wand2, X } from "lucide-react";

import { api, ApiError, type SummaryTemplate } from "../lib/api";

/**
 * The prompts that summarising can use.
 *
 * # Why this screen exists
 *
 * Three templates ship seeded, `SummaryTemplatePicker` lists them beside a summary, and running one
 * works. Making one did not: the create, edit and delete endpoints existed and no screen called
 * them, so "summary templates" in practice meant "three summary templates". A prompt someone cannot
 * change is a setting the product chose for them.
 *
 * # Built-ins are editable and not deletable
 *
 * That asymmetry is the engine's, not this screen's invention — `SummaryRepository::delete_template`
 * refuses a seeded row, and editing one keeps `is_builtin` set. So the delete control is hidden on
 * those rather than shown and failing, which is the difference between a rule and a trap.
 *
 * There is no reset-to-original, because nothing stores the original text. Worth knowing before
 * editing a built-in, so it says so.
 */
export function SummaryTemplateSettings() {
  const [templates, setTemplates] = useState<SummaryTemplate[]>([]);
  const [editing, setEditing] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState({ name: "", prompt: "" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setTemplates(await api.summaryTemplates());
    } catch {
      // A list that will not load is not worth a banner over the settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (run: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await run();
      await load();
      return true;
    } catch (e) {
      // The engine refuses an empty name or prompt with a reason worth reading, so it is shown
      // rather than replaced with something generic.
      setError(e instanceof ApiError ? e.message : "Could not save that.");
      return false;
    } finally {
      setBusy(false);
    }
  };

  const startEdit = (template: SummaryTemplate) => {
    setAdding(false);
    setEditing(template.id);
    setDraft({ name: template.name, prompt: template.prompt });
    setError(null);
  };

  const startAdd = () => {
    setEditing(null);
    setAdding(true);
    setDraft({ name: "", prompt: "" });
    setError(null);
  };

  const cancel = () => {
    setEditing(null);
    setAdding(false);
    setError(null);
  };

  const save = async () => {
    const ok = await act(() =>
      editing
        ? api.updateSummaryTemplate(editing, draft.name, draft.prompt)
        : api.createSummaryTemplate(draft.name, draft.prompt),
    );
    // Only close on success. Closing on a refusal would discard what the user typed along with the
    // explanation of why it was refused.
    if (ok) cancel();
  };

  const remove = (template: SummaryTemplate) => {
    const ok = window.confirm(
      `Delete the "${template.name}" template?\n\nSummaries already made with it are not affected.`,
    );
    if (ok) void act(() => api.deleteSummaryTemplate(template.id));
  };

  /** Shared by the add and edit forms — they differ only in which call they end in. */
  const form = (
    <div className="space-y-2 px-4 py-3">
      <input
        value={draft.name}
        onChange={(event) => setDraft((d) => ({ ...d, name: event.target.value }))}
        placeholder="Name, as it appears on the button"
        aria-label="Template name"
        className="w-full rounded border border-hairline bg-surface px-2 py-1.5 text-[13px] text-ink
                   placeholder:text-ink-faint"
      />
      <textarea
        value={draft.prompt}
        onChange={(event) => setDraft((d) => ({ ...d, prompt: event.target.value }))}
        rows={4}
        placeholder="What the model should do with the transcript — e.g. “List only the decisions, with who made each one.”"
        aria-label="Template prompt"
        className="w-full resize-y rounded border border-hairline bg-surface px-2 py-1.5 text-[12.5px]
                   leading-relaxed text-ink placeholder:text-ink-faint"
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => void save()}
          disabled={busy}
          className="flex items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                     text-[12px] text-ink transition hover:bg-surface
                     disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? (
            <Loader2 size={12} className="animate-spin" aria-hidden />
          ) : (
            <Check size={12} aria-hidden />
          )}
          Save
        </button>
        <button
          type="button"
          onClick={cancel}
          disabled={busy}
          className="flex items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                     text-[12px] text-ink-muted transition hover:bg-surface hover:text-ink
                     disabled:opacity-40"
        >
          <X size={12} aria-hidden />
          Cancel
        </button>
      </div>
    </div>
  );

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Wand2 size={14} className="text-ink-faint" aria-hidden />
        Summary templates
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Each template is one prompt. They appear under any summary, so a meeting can be summarised
        more than one way — running another adds a summary rather than replacing the one you have.
      </p>

      <div className="card divide-y divide-hairline overflow-hidden">
        {templates.map((template) =>
          editing === template.id ? (
            <div key={template.id}>{form}</div>
          ) : (
            <div key={template.id} className="group flex items-start gap-3 px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="text-[13px] font-medium text-ink">{template.name}</span>
                  {template.is_builtin && (
                    <span className="rounded-full bg-overlay px-1.5 py-0.5 text-[10.5px] text-ink-muted">
                      Built in
                    </span>
                  )}
                </div>
                {/* The prompt in full, not truncated. It is the entire content of the thing being
                    listed, and a template you cannot read is one you cannot decide about. */}
                <p className="mt-0.5 whitespace-pre-wrap text-[12px] leading-relaxed text-ink-muted">
                  {template.prompt}
                </p>
              </div>

              <div className="flex shrink-0 items-center gap-1">
                <button
                  type="button"
                  onClick={() => startEdit(template)}
                  disabled={busy}
                  aria-label={`Edit ${template.name}`}
                  className="rounded p-1 text-ink-faint opacity-0 transition hover:bg-overlay
                             hover:text-ink group-hover:opacity-100 focus-visible:opacity-100
                             disabled:opacity-40"
                >
                  <Pencil size={13} aria-hidden />
                </button>

                {/* Hidden rather than disabled on a built-in: the engine refuses it, and a control
                    that is present but never works reads as a bug. */}
                {!template.is_builtin && (
                  <button
                    type="button"
                    onClick={() => remove(template)}
                    disabled={busy}
                    aria-label={`Delete ${template.name}`}
                    className="rounded p-1 text-ink-faint opacity-0 transition hover:bg-overlay
                               hover:text-warn-text group-hover:opacity-100 focus-visible:opacity-100
                               disabled:opacity-40"
                  >
                    <Trash2 size={13} aria-hidden />
                  </button>
                )}
              </div>
            </div>
          ),
        )}

        {adding ? (
          form
        ) : (
          <button
            type="button"
            onClick={startAdd}
            disabled={busy}
            className="flex w-full items-center gap-1.5 px-4 py-2.5 text-left text-[12.5px]
                       text-ink-muted transition hover:bg-overlay hover:text-ink
                       disabled:opacity-40"
          >
            <Plus size={13} aria-hidden />
            New template
          </button>
        )}
      </div>

      <p className="mt-2 text-[11px] leading-snug text-ink-faint">
        Built-in templates can be edited but not deleted, and the original wording is not kept — so
        an edit to one cannot be undone from here.
      </p>

      {error && (
        <p role="alert" className="mt-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </section>
  );
}
