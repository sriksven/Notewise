import { useCallback, useEffect, useState } from "react";
import {
  AlertTriangle,
  Check,
  Keyboard,
  Loader2,
  Mic,
  Sparkles,
  Square,
  TextCursorInput,
  X,
} from "lucide-react";

import {
  api,
  ApiError,
  type AssistantCapabilities,
  type AssistantPermission,
  type Dictated,
  type Completion,
  type TypingActivity,
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

  const [overlayHotkey, setOverlayHotkey] = useState("");
  const [typing, setTyping] = useState<TypingActivity | null>(null);
  const [completionDraft, setCompletionDraft] = useState("");
  const [completion, setCompletion] = useState<Completion | null>(null);
  const [trying, setTrying] = useState(false);
  const [screen, setScreen] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [found, status, watch] = await Promise.all([
        api.assistant(),
        api.dictationStatus(),
        api.typingActivity().catch(() => null),
      ]);
      setCapabilities(found);
      setHotkey(found.hotkey);
      setOverlayHotkey(
        found.hotkeys.find((entry) => entry.feature === "overlay")?.hotkey ?? "",
      );
      setListening(status.listening);
      setTyping(watch?.activity ?? null);
    } catch {
      // A capabilities read that fails is not worth a banner over the whole settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const saveHotkey = async (next: string, feature: "dictation" | "overlay" = "dictation") => {
    setSaving(true);
    setError(null);
    try {
      const saved = await api.setAssistantHotkey(next, capabilities?.mode, feature);
      if (feature === "dictation") setHotkey(saved.hotkey);
      else setOverlayHotkey(saved.hotkey);
      setWarning(saved.warning);
    } catch (e) {
      // The engine refuses a combination another feature already holds, and names it. Reload so the
      // field shows what is actually stored rather than what was typed.
      setError(e instanceof ApiError ? e.message : "Could not save that hotkey.");
      await load();
    } finally {
      setSaving(false);
    }
  };

  /** What the panel would be shown, before anybody uses it. */
  const showWhatItSees = async () => {
    setError(null);
    try {
      const found = await api.screenContext();
      setScreen(found.empty ? "Nothing readable is on screen right now." : found.prompt);
    } catch (e) {
      setScreen(null);
      setError(e instanceof ApiError ? e.message : "Could not read the screen.");
    }
  };

  const toggleTyping = async () => {
    setError(null);
    try {
      const next = typing?.running
        ? await api.stopTypingMonitor()
        : await api.startTypingMonitor();
      setTyping(next.activity);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not change that.");
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

  /**
   * Show what would be suggested for a sentence typed here.
   *
   * The same reason the dictation button exists: the real feature needs Input Monitoring, a pause in
   * typing, and a model — three things that can each be wrong on their own — and nothing else in the
   * app can tell you which. `force` skips the pause and the rate limit and nothing else, so an empty
   * field still has nothing to suggest.
   *
   * The engine reads the focused window when no text is passed. Text is passed here, because the
   * focused window while this screen is open is this screen.
   */
  const trySuggestion = async () => {
    const text = completionDraft.trim();
    if (!text) return;

    setTrying(true);
    setError(null);
    setCompletion(null);
    try {
      setCompletion(await api.suggestCompletion({ text, force: true }));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not get a suggestion.");
    } finally {
      setTrying(false);
    }
  };

  /**
   * Stop listening and throw the words away.
   *
   * The only other way out of a live session is "Stop and insert", which types wherever the cursor
   * happens to be. Someone who started this by accident, or said the wrong thing, needs an exit
   * that does not put text into another application — the engine has had one all along and nothing
   * offered it.
   */
  const discard = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.cancelDictation();
      setResult(null);
      setListening(false);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not stop listening.");
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
            <div className="flex items-center gap-2">
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

              {/* Only while listening, and never the primary action — insert is what the feature is
                  for. But it has to be here, or the only way out types into another app. */}
              {listening && (
                <button
                  type="button"
                  onClick={() => void discard()}
                  disabled={busy}
                  className="flex items-center gap-1.5 rounded-full border border-hairline px-3 py-1.5
                             text-[12px] text-ink-muted transition hover:bg-surface hover:text-ink
                             disabled:opacity-50"
                >
                  <X size={12} aria-hidden />
                  Discard
                </button>
              )}
            </div>
            <p className="mt-1.5 text-[11.5px] text-ink-faint">
              {listening
                ? "Listening. Click somewhere you want the text first — it goes wherever the cursor is when you stop, or discard it and nothing is inserted."
                : "Checks whether dictation works on this machine, which is the one thing no test can."}
            </p>

            {result && <Outcome result={result} canInsert={capabilities.can_insert} />}
          </div>
        )}
      </div>

      {/* 9b and 9c: one hotkey, because from the user's side it is one gesture — the panel offers
          actions when something is highlighted and answers questions when nothing is. */}
      <h3 className="mb-1 mt-4 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
        <Sparkles size={12} className="text-ink-faint" aria-hidden />
        Assistant panel
      </h3>
      <p className="mb-2 text-[12px] leading-relaxed text-ink-muted">
        Opens over whatever you are working in. Ask about the window in front of you, or highlight
        something first and rewrite, shorten, or translate it in place. What gets sent to the model
        is shown in the panel every time.
      </p>
      <div className="space-y-2 rounded-lg border border-hairline p-3">
        <label className="block">
          <span className="mb-1 flex items-center gap-1.5 text-[12px] text-ink">
            <Keyboard size={12} className="text-ink-faint" aria-hidden />
            Hotkey
          </span>
          <input
            value={overlayHotkey}
            onChange={(event) => setOverlayHotkey(event.target.value)}
            onBlur={() => void saveHotkey(overlayHotkey, "overlay")}
            placeholder="super+shift+a"
            spellCheck={false}
            className="w-56 rounded border border-hairline bg-transparent px-2 py-1 font-mono
                       text-[12px] text-ink placeholder:text-ink-faint"
          />
        </label>
        <p className="text-[11.5px] leading-relaxed text-ink-faint">
          Reading the window in front needs the Accessibility permission above. Reading text that
          is only on screen as pixels — a PDF, a screenshot — additionally needs a signed build,
          which a development build is not.
        </p>

        {/* "What does it see" answerable before the panel is used rather than after. This is the
            surface that reads a window belonging to another application, so it should be possible
            to check that without having to ask it a question first. */}
        <div className="border-t border-hairline pt-2">
          <button
            type="button"
            onClick={() => void showWhatItSees()}
            className="btn-ghost px-2.5 py-1 text-[11.5px]"
          >
            Show me what it would read
          </button>

          {screen !== null && (
            <pre className="mt-1.5 max-h-40 overflow-auto whitespace-pre-wrap rounded bg-overlay p-2
                            font-mono text-[11px] leading-snug text-ink-muted">
              {screen}
            </pre>
          )}
        </div>
      </div>

      {/* 9d, with its limits stated rather than implied. */}
      <h3 className="mb-1 mt-4 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
        <TextCursorInput size={12} className="text-ink-faint" aria-hidden />
        Inline suggestions
      </h3>
      <p className="mb-2 text-[12px] leading-relaxed text-ink-muted">
        Notices when you stop typing and offers to finish the sentence. The suggestion appears in
        Notewise, not as greyed-out text inside the other app — macOS gives no way to draw into
        another application&rsquo;s window, and pretending otherwise would show you nothing.
      </p>
      <div className="space-y-2 rounded-lg border border-hairline p-3">
        <label className="flex cursor-pointer items-start gap-2">
          <input
            type="checkbox"
            checked={typing?.running ?? false}
            onChange={() => void toggleTyping()}
            className="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
          />
          <span className="min-w-0">
            <span className="block text-[12px] text-ink">Watch for typing pauses</span>
            <span className="block text-[11.5px] leading-relaxed text-ink-faint">
              Needs Input Monitoring, the most invasive permission macOS has. Notewise records only
              that a key was pressed and when — no key codes, no characters, nothing kept. Off
              until you turn it on, and never turned on by another feature.
            </span>
          </span>
        </label>

        {typing?.running && (
          <p className="text-[11.5px] text-ink-faint">
            Watching. {typing.keystrokes} keystroke{typing.keystrokes === 1 ? "" : "s"} counted this
            session.
          </p>
        )}

        {/* Independent of the toggle above on purpose: this is how you find out whether a model will
            suggest anything at all, and needing Input Monitoring first to learn that would be
            backwards. */}
        <div className="border-t border-hairline pt-2">
          <label className="mb-1 block text-[11.5px] text-ink-faint" htmlFor="completion-try">
            Try it — type half a sentence
          </label>
          <div className="flex items-center gap-2">
            <input
              id="completion-try"
              value={completionDraft}
              onChange={(event) => setCompletionDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void trySuggestion();
                }
              }}
              placeholder="The main risk with this approach is"
              className="min-w-0 flex-1 rounded border border-hairline bg-transparent px-2 py-1
                         text-[12px] text-ink placeholder:text-ink-faint"
            />
            <button
              type="button"
              onClick={() => void trySuggestion()}
              disabled={trying || !completionDraft.trim()}
              className="flex shrink-0 items-center gap-1 rounded-full border border-hairline
                         px-2.5 py-1 text-[11.5px] text-ink-muted transition hover:bg-surface
                         hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
            >
              {trying ? (
                <Loader2 size={11} className="animate-spin" aria-hidden />
              ) : (
                <TextCursorInput size={11} aria-hidden />
              )}
              Suggest
            </button>
          </div>

          {completion && (
            <p className="mt-1.5 text-[11.5px] leading-relaxed">
              {completion.suggestion ? (
                <>
                  <span className="text-ink-faint">{completion.text}</span>
                  <span className="font-medium text-ink">{completion.suggestion}</span>
                  {completion.model && (
                    <span className="block text-ink-faint">via {completion.model}</span>
                  )}
                </>
              ) : (
                // The decision, not a shrug. `too_short` and `too_long` are the policy working;
                // anything else with no suggestion means the model had nothing to add.
                <span className="text-ink-faint">
                  Nothing suggested — {completion.decision.replace(/_/g, " ")}.
                </span>
              )}
            </p>
          )}
        </div>
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
