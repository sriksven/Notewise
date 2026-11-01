import { useCallback, useEffect, useRef, useState } from "react";

import { api, ApiError } from "./api";

/**
 * Asking out loud.
 *
 * The engine's dictation, pointed at a box in this app instead of at the cursor. It starts with
 * `insert: false`, so the words come back rather than being typed through the OS, and with
 * `partials: true`, so the status carries them while they are being said. Whisper runs on this
 * machine, so a spoken question leaves it no more than a typed one does.
 *
 * What shows while listening is provisional: a fast guess at the phrase in progress, revised as
 * more of it arrives. The stop decodes the last phrase properly, and only that text is kept.
 */
export type VoiceState = "idle" | "starting" | "listening" | "finishing";

/** How often the words are read while listening. A word should show about as it is finished. */
const POLL_MS = 250;

export interface VoiceInput {
  /** Whether this build can dictate. Null until the engine has said. */
  supported: boolean | null;
  state: VoiceState;
  /** Everything heard so far, as one line. Provisional until the stop. */
  heard: string;
  error: string | null;
  /** Start listening, or stop and hand the words over. */
  toggle: () => void;
  /** Stop listening and throw the words away. */
  cancel: () => void;
}

/** The kept words and the guess after them, as one line. */
export function joinHeard(committed: string, interim: string): string {
  return [committed.trim(), interim.trim()].filter(Boolean).join(" ");
}

/** What goes between what was typed and what is said after it. */
export function spaceBefore(draft: string): string {
  return draft.length === 0 || /\s$/.test(draft) ? "" : " ";
}

/** A spoken question added to whatever was already typed, so speaking never erases typing. */
export function appendSpoken(draft: string, spoken: string): string {
  const words = spoken.trim();
  if (!words) return draft;
  return draft + spaceBefore(draft) + words;
}

/**
 * Listen for a question and hand over what was said.
 *
 * `onHeard` gets the final words once, after the stop. Closing whatever this lives in while it is
 * listening throws the words away and closes the microphone: a closed panel must not keep
 * recording behind it.
 */
export function useVoiceInput(onHeard: (text: string) => void): VoiceInput {
  const [supported, setSupported] = useState<boolean | null>(null);
  const [state, setState] = useState<VoiceState>("idle");
  const [heard, setHeard] = useState("");
  const [error, setError] = useState<string | null>(null);

  // The state as the async work below sees it. A read that lands after the stop must not paint
  // old words over the final ones, and an unmount has to know whether a session is open.
  const live = useRef<VoiceState>("idle");
  const mounted = useRef(true);
  const deliver = useRef(onHeard);
  deliver.current = onHeard;

  const move = useCallback((next: VoiceState) => {
    live.current = next;
    setState(next);
  }, []);

  useEffect(() => {
    mounted.current = true;
    api
      .dictationStatus()
      .then((status) => mounted.current && setSupported(status.supported))
      .catch(() => mounted.current && setSupported(false));

    return () => {
      mounted.current = false;
      if (live.current === "listening") void api.cancelDictation().catch(() => {});
    };
  }, []);

  useEffect(() => {
    if (state !== "listening") return;

    const timer = window.setInterval(() => {
      api
        .dictationStatus()
        .then((status) => {
          if (!mounted.current || live.current !== "listening") return;
          if (!status.listening) {
            // Ended somewhere else: the dictation hotkey stops whatever is listening.
            move("idle");
            setHeard("");
            setError("Stopped listening.");
            return;
          }
          setHeard(joinHeard(status.committed ?? "", status.interim ?? ""));
        })
        // One missed read costs a quarter of a second of lag, and the next one will try again.
        .catch(() => {});
    }, POLL_MS);

    return () => window.clearInterval(timer);
  }, [state, move]);

  const start = useCallback(async () => {
    setError(null);
    setHeard("");
    move("starting");
    try {
      // Raw, never cleaned: the question goes to a model anyway, and cleaning first would be a
      // second round trip, and an upload on a hosted backend, before it is even asked.
      await api.startDictation({ mode: "raw", insert: false, partials: true });
      if (!mounted.current) {
        // Closed while the microphone was opening.
        void api.cancelDictation().catch(() => {});
        return;
      }
      move("listening");
    } catch (e) {
      if (!mounted.current) return;
      move("idle");
      setError(e instanceof ApiError ? e.message : "Could not start listening.");
    }
  }, [move]);

  const stop = useCallback(async () => {
    move("finishing");
    try {
      const dictated = await api.stopDictation();
      if (!mounted.current) return;
      const words = dictated.text.trim();
      if (words) deliver.current(words);
      else setError(dictated.note ?? "Nothing was heard.");
    } catch (e) {
      if (mounted.current) {
        setError(e instanceof ApiError ? e.message : "Could not finish listening.");
      }
    } finally {
      if (mounted.current) {
        setHeard("");
        move("idle");
      }
    }
  }, [move]);

  const toggle = useCallback(() => {
    if (live.current === "idle") void start();
    else if (live.current === "listening") void stop();
  }, [start, stop]);

  const cancel = useCallback(() => {
    if (live.current !== "listening") return;
    move("idle");
    setHeard("");
    void api.cancelDictation().catch(() => {});
  }, [move]);

  return { supported, state, heard, error, toggle, cancel };
}
