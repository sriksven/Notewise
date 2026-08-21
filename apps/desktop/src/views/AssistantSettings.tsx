import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Check, Keyboard, Loader2, Mic, Square } from "lucide-react";

import {
  api,
  ApiError,
  type AssistantCapabilities,
  type AssistantPermission,
  type Dictated,
} from "../lib/api";

/**
 * Dictation, and what the operating system is letting it do.
 *
 * # Why the permissions are the first thing on the screen
 *
 * Accessibility is one of the two most alarming grants on macOS, and asking for it is a trust event
 * for a product sold on privacy. So this screen says what each grant is for before it asks, names
 * the pane, and says that macOS only applies it at the next launch — a feature that silently stays
 * broken until the app restarts reads as a feature that does not work.
 *
 * # Why there is a try-it control
 *
 * Every native path here needs a grant no test can hold. This is where somebody finds out whether
 * it actually works on their machine, and it reports which tier ran — including when the clipboard
 * was borrowed, which the user is entitled to know at the time rather than at their next paste.
 */
export function AssistantSettings() {
  const [capabilities, setCapabilities] = useState<AssistantCapabilities | null>(null);
  const [hotkey, setHotkey] = useState("");
  const [warning, setWarning] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const [listening, setListening] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Dictated | null>(null);

  const load = useCallback(async () => {
    try {
      const [found, status] = await Promise.all([api.assistant(), api.dictationStatus()]);
      setCapabilities(found);
      setHotkey(found.hotkey);
      setListening(status.listening);
    } catch {
      // A capabilities read that fails is not worth a banner over the whole settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const saveHotkey = async (next: string) => {
    setSaving(true);
    setError(null);
    try {
      const saved = await api.setAssistantHotkey(next, capabilities?.mode);
      setHotkey(saved.hotkey);
      setWarning(saved.warning);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not save that hotkey.");
      setHotkey(capabilities?.hotkey ?? "");
    } finally {
      setSaving(false);
    }
  };

  const setMode = async (mode: "raw" | "cleaned") => {
    setCapabilities((current) => (current ? { ...current, mode } : current));
    try {
      await api.setAssistantHotkey(hotkey, mode);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not save that.");
      await load();
    }
  };

  const toggle = async () => {
    setBusy(true);
    setError(null);
    try {
      if (listening) {
        const dictated = await api.stopDictation();
        setResult(dictated);
        setListening(false);
      } else {
        setResult(null);
        await api.startDictation({ mode: capabilities?.mode });
        setListening(true);
      }
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "That did not work.");
      // The engine is the source of truth about whether it is listening; a failed toggle means
      // this component's idea of the state is the one that is wrong.
      await load();
    } finally {
      setBusy(false);
    }
  };

  if (!capabilities) return null;

  const blocked = capabilities.permissions.filter((p) => p.status !== "granted");

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Mic size={13} className="text-ink-faint" aria-hidden />
        Dictation
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Press a key, talk, and the words appear wherever your cursor is — in any app. Transcribed on
        this machine by the same model that transcribes your meetings. Nothing is sent anywhere
        unless you choose the cleaned mode.
      </p>

      {error && (
        <div
          role="alert"
          className="mb-3 rounded-lg border border-warn-line bg-warn px-3 py-2 text-[12.5px] text-warn-text"
        >
          {error}
        </div>
      )}

      {!capabilities.can_dictate && (
        <p className="mb-3 rounded-lg border border-hairline bg-overlay px-3 py-2 text-[12.5px] text-ink-muted">
          {capabilities.reason ?? "This build cannot dictate."}
        </p>
      )}

      {/* Permissions first: this is a trust event, and it should read as one. */}
      {blocked.length > 0 && (
        <ul className="mb-3 space-y-2">
          {blocked.map((permission) => (
            <PermissionNote key={permission.capability} permission={permission} />
          ))}
        </ul>
      )}

      <div className="space-y-3 rounded-lg border border-hairline p-3">
        <label className="block">
          <span className="mb-1 flex items-center gap-1.5 text-[12px] text-ink">
            <Keyboard size={12} className="text-ink-faint" aria-hidden />
            Hotkey
          </span>
          <input
            value={hotkey}
            onChange={(event) => setHotkey(event.target.value)}
            onBlur={() => hotkey !== capabilities.hotkey && void saveHotkey(hotkey)}
            placeholder="super+shift+d"
            spellCheck={false}
            className="w-56 rounded border border-hairline bg-transparent px-2 py-1 font-mono
                       text-[12px] text-ink placeholder:text-ink-faint"
          />
          <span className="ml-2 text-[11.5px] text-ink-faint">
            {saving ? "saving…" : "cmd, ctrl, alt, shift — press it once to start, again to stop"}
          </span>
        </label>

        {warning && (
          <p className="flex items-start gap-1.5 text-[11.5px] leading-relaxed text-warn-text">
            <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
            {warning}
          </p>
        )}

        <fieldset>
          <legend className="mb-1 text-[12px] text-ink">What to insert</legend>
          <div className="space-y-1">
            <ModeChoice
              checked={capabilities.mode === "raw"}
              onSelect={() => void setMode("raw")}
              title="Exactly what you said"
              description="No model beyond transcription. Works with nothing else configured, and nothing leaves this machine."
            />
            <ModeChoice
              checked={capabilities.mode === "cleaned"}
              onSelect={() => void setMode("cleaned")}
              title="Punctuation fixed"
              description="Sends the transcript to your configured model to add punctuation and capitals. Your words are never rewritten — if the model tries, the raw text is used instead."
            />
          </div>
        </fieldset>

        {capabilities.can_dictate && (
          <div className="border-t border-hairline pt-3">
            <button
              type="button"
              onClick={() => void toggle()}
              disabled={busy}
              className={`flex items-center gap-1.5 rounded-full px-3 py-1.5 text-[12px] transition
                          disabled:opacity-50 ${
                            listening
                              ? "bg-warn text-warn-text"
                              : "bg-accent text-accent-on hover:opacity-90"
                          }`}
            >
              {busy ? (
                <Loader2 size={12} className="animate-spin" aria-hidden />
              ) : listening ? (
                <Square size={11} aria-hidden />
              ) : (
                <Mic size={12} aria-hidden />
              )}
              {listening ? "Stop and insert" : "Try it"}
            </button>
            <p className="mt-1.5 text-[11.5px] text-ink-faint">
              {listening
                ? "Listening. Click somewhere you want the text first — it goes wherever the cursor is when you stop."
                : "Checks whether dictation works on this machine, which is the one thing no test can."}
            </p>

            {result && <Outcome result={result} canInsert={capabilities.can_insert} />}
          </div>
        )}
      </div>
    </section>
  );
}

function ModeChoice({
  checked,
  onSelect,
  title,
  description,
}: {
  checked: boolean;
  onSelect: () => void;
  title: string;
  description: string;
}) {
  return (
    <label className="flex cursor-pointer items-start gap-2">
      <input
        type="radio"
        checked={checked}
        onChange={onSelect}
        className="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
      />
      <span className="min-w-0">
        <span className="block text-[12px] text-ink">{title}</span>
        <span className="block text-[11.5px] leading-relaxed text-ink-faint">{description}</span>
      </span>
    </label>
  );
}

function PermissionNote({ permission }: { permission: AssistantPermission }) {
  return (
    <li className="rounded-lg border border-warn-line bg-warn px-3 py-2">
      <p className="flex items-center gap-1.5 text-[12.5px] font-medium text-warn-text">
        <AlertTriangle size={12} aria-hidden />
        {permission.label} is not enabled
      </p>
      {permission.how_to_grant && (
        <p className="mt-1 text-[11.5px] leading-relaxed text-warn-text">
          {permission.how_to_grant}
        </p>
      )}
      <button
        type="button"
        onClick={() => {
          window.location.href = permission.settings_url;
        }}
        className="mt-1.5 rounded-full border border-warn-line px-2 py-0.5 text-[11.5px] text-warn-text
                   transition hover:bg-warn-text/10"
      >
        Open the {permission.label} settings
      </button>
    </li>
  );
}

/**
 * What happened to the last dictation.
 *
 * Shows the text whatever the outcome. If insertion refused, the words were still said, and hiding
 * them because they could not be placed would lose them.
 */
function Outcome({ result, canInsert }: { result: Dictated; canInsert: boolean }) {
  const placed = result.insertion === "accessibility";
  const viaClipboard =
    typeof result.insertion === "object" && result.insertion !== null && "clipboard" in result.insertion;

  return (
    <div className="mt-2.5 rounded-lg border border-hairline bg-overlay p-2.5">
      {result.text ? (
        <>
          <p className="mb-1 flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
            {placed || viaClipboard ? (
              <Check size={11} className="text-ok-text" aria-hidden />
            ) : null}
            Heard
          </p>
          <p className="whitespace-pre-wrap text-[12.5px] leading-relaxed text-ink">{result.text}</p>

          {result.raw_text && (
            <details className="mt-1.5">
              <summary className="cursor-pointer text-[11.5px] text-ink-faint hover:text-ink">
                Before punctuation was fixed
              </summary>
              <p className="mt-1 whitespace-pre-wrap text-[12px] text-ink-muted">
                {result.raw_text}
              </p>
            </details>
          )}
        </>
      ) : (
        <p className="text-[12.5px] text-ink-muted">Nothing was heard.</p>
      )}

      {result.note && (
        <p className="mt-1.5 text-[11.5px] leading-relaxed text-ink-faint">{result.note}</p>
      )}

      {!canInsert && result.text && (
        <p className="mt-1.5 text-[11.5px] leading-relaxed text-ink-faint">
          This build cannot type into other applications, so the text stayed here.
        </p>
      )}

      <p className="mt-1.5 text-[11px] text-ink-faint">
        {(result.duration_ms / 1000).toFixed(1)}s of audio
      </p>
    </div>
  );
}
