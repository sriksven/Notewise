import { useEffect, useState } from "react";
import { Cloud, HardDrive, RefreshCw } from "lucide-react";

import { api, ApiError, type BackendInfo } from "../../lib/api";

interface BackendStepProps {
  satisfied: boolean;
  onChanged: () => Promise<void>;
}

/** Read by the engine from its own environment, never sent over HTTP. */
const KEY_VARIABLES = [
  "ANTHROPIC_API_KEY",
  "GEMINI_API_KEY",
  "GROQ_API_KEY",
  "OPENROUTER_API_KEY",
];

export function BackendStep({ satisfied, onChanged }: BackendStepProps) {
  const [active, setActive] = useState<{
    kind: string;
    model: string;
    is_local: boolean;
  } | null>(null);
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      const { backends: list, active: current } = await api.backends();
      setBackends(list);
      setActive(current);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not read the backend list.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const recheck = async () => {
    setBusy(true);
    try {
      await load();
      await onChanged();
    } finally {
      setBusy(false);
    }
  };

  // The backend actually running, matched by kind. Matching on `is_local` instead would find
  // whichever local backend happens to come first in the list — labelling a live Ollama as
  // "Mock (no model)".
  const running = backends.find((b) => b.kind === active?.kind);
  const isLocal = active?.is_local ?? true;

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-neutral-900">
        Summaries and chat
      </h1>
      <p className="mt-2 max-w-md text-[14px] text-neutral-500">
        Transcription is local either way. This decides where the language model runs.
      </p>

      {error && (
        <div
          role="alert"
          className="mt-6 w-full max-w-md rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-left text-[13px] text-amber-900"
        >
          {error}
        </div>
      )}

      <div className="mt-8 w-full max-w-md space-y-3 text-left">
        <div
          className={`rounded-xl border p-4 ${
            satisfied ? "border-emerald-200 bg-emerald-50" : "border-hairline bg-white"
          }`}
        >
          <div className="flex items-start gap-3">
            {isLocal ? (
              <HardDrive size={16} className="mt-0.5 shrink-0 text-emerald-600" aria-hidden />
            ) : (
              <Cloud size={16} className="mt-0.5 shrink-0 text-amber-600" aria-hidden />
            )}

            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-medium text-neutral-900">
                {running?.label ?? "Current backend"}
              </div>

              <div className="mt-0.5 flex items-center gap-1.5 text-[12px]">
                <span
                  aria-hidden
                  className={`h-1.5 w-1.5 rounded-full ${
                    satisfied ? "bg-emerald-500" : "bg-amber-500"
                  }`}
                />
                <span className={satisfied ? "text-emerald-800" : "text-amber-800"}>
                  {satisfied
                    ? `Reachable — ${active?.model ?? "ready"}`
                    : "Not reachable. Is it running?"}
                </span>
              </div>

              {/* The privacy claim follows the backend actually in use. Printing "nothing
                  leaves this machine" under a hosted provider would be the one lie this
                  product cannot afford. */}
              <p className="mt-2 text-[12px] text-neutral-500">
                {isLocal
                  ? "Nothing leaves this machine. Notewise does not install or update this — start it, then re-check."
                  : "Transcripts are sent to this provider. Switch in Settings if you would rather keep them local."}
              </p>
            </div>

            <button
              type="button"
              onClick={() => void recheck()}
              disabled={busy}
              className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline
                         bg-white px-3 py-1.5 text-[12px] text-neutral-700 transition
                         hover:bg-neutral-50 disabled:opacity-50"
            >
              <RefreshCw size={13} className={busy ? "animate-spin" : ""} aria-hidden />
              Re-check
            </button>
          </div>
        </div>

        <div className="rounded-xl border border-hairline bg-white p-4">
          <div className="flex items-start gap-3">
            <Cloud size={16} className="mt-0.5 shrink-0 text-neutral-400" aria-hidden />

            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-medium text-neutral-900">Bring your own key</div>
              <p className="mt-0.5 text-[12px] text-neutral-500">
                Transcripts are sent to the provider you choose. Set one of these in the
                engine's environment and restart, then re-check:
              </p>

              <ul className="mt-2 flex flex-wrap gap-1.5">
                {KEY_VARIABLES.map((name) => (
                  <li
                    key={name}
                    className="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-[11px] text-neutral-600"
                  >
                    {name}
                  </li>
                ))}
              </ul>

              <p className="mt-2 text-[11px] text-neutral-400">
                Keys are read by the engine directly. Notewise never sends one over HTTP, not
                even on loopback.
              </p>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
