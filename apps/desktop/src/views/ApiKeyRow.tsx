import { useState } from "react";
import { Check, KeyRound, Loader2, Trash2 } from "lucide-react";

import { api, ApiError, type BackendInfo } from "../lib/api";

interface Props {
  backend: BackendInfo;
  onChanged: () => void;
}

/** Where each provider issues keys, so nobody has to go looking. */
const CONSOLE: Record<string, string> = {
  anthropic: "console.anthropic.com",
  gemini: "aistudio.google.com",
  groq: "console.groq.com",
  openrouter: "openrouter.ai/keys",
};

/**
 * Add or remove one provider's API key.
 *
 * The key is written to the OS keychain and is never readable back — the row can only ever say
 * whether one exists. That is why there is no "show key" affordance and no value in the input
 * once saved: a key you can reveal is a key in the next screenshot.
 *
 * Saving switches to the provider straight away. Typing a key and then having to find the
 * provider in a separate menu is two steps for one intention, and the step people miss.
 */
export function ApiKeyRow({ backend, onChanged }: Props) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    if (!value.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await api.setApiKey(backend.kind, value.trim());
      // Cleared immediately: there is no reason for the key to stay in a DOM node.
      setValue("");
      onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not save the key.");
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.deleteApiKey(backend.kind);
      onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not remove the key.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="border-t border-hairline bg-overlay px-3 py-2.5">
      {backend.has_key ? (
        <div className="flex items-center gap-2">
          <Check size={14} className="shrink-0 text-ok-text" aria-hidden />
          <span className="flex-1 text-[12px] text-ink-muted">
            A key is saved in your keychain. It is never shown again or sent anywhere but{" "}
            {backend.label}.
          </span>
          <button
            type="button"
            onClick={() => void remove()}
            disabled={busy}
            className="flex shrink-0 items-center gap-1 rounded-full border border-hairline px-2.5 py-1
                       text-[12px] text-ink-muted transition hover:bg-surface hover:text-ink
                       disabled:opacity-50"
          >
            {busy ? (
              <Loader2 size={12} className="animate-spin" aria-hidden />
            ) : (
              <Trash2 size={12} aria-hidden />
            )}
            Remove
          </button>
        </div>
      ) : (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
        >
          <label className="mb-1.5 flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
            <KeyRound size={12} aria-hidden />
            {backend.label} API key
          </label>
          <div className="flex gap-2">
            <input
              type="password"
              value={value}
              onChange={(event) => setValue(event.target.value)}
              placeholder="Paste your key"
              autoComplete="off"
              spellCheck={false}
              className="field flex-1"
            />
            <button type="submit" disabled={busy || !value.trim()} className="btn-accent shrink-0">
              {busy && <Loader2 size={13} className="animate-spin" aria-hidden />}
              Save & use
            </button>
          </div>
          <p className="mt-1.5 text-[11px] leading-snug text-ink-faint">
            Stored in your OS keychain, never in the database and never in a log. Get one at{" "}
            <span className="text-ink-muted">{CONSOLE[backend.kind] ?? "your provider"}</span>.
          </p>
        </form>
      )}

      {error && (
        <p role="alert" className="mt-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </div>
  );
}
