import { useEffect, useState } from "react";
import { Check, ChevronDown, Download, Loader2 } from "lucide-react";

import { api, type ModelInfo } from "../../lib/api";
import { size } from "../../lib/format";
import { useModelDownload } from "../../lib/useModelDownload";

interface ModelStepProps {
  satisfied: boolean;
  /** Re-read readiness once the download lands, so the gate opens. */
  onChanged: () => Promise<void>;
}

/**
 * Pick a speech model.
 *
 * This screen used to show exactly one card — whichever model the registry marked recommended —
 * with no indication that eight others existed. That reads as a broken list rather than a
 * decision already made for you, and it left anyone who needs another language or better
 * accuracy with no way forward from the one screen that is about models.
 *
 * The recommendation stays: the list opens collapsed on the recommended model, because on a
 * first run the right answer is "press the button" and a wall of nine options is worse than
 * one. Everything else is one click away, and every option says what it is for.
 */
export function ModelStep({ satisfied, onChanged }: ModelStepProps) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [listError, setListError] = useState<string | null>(null);

  const { downloading, progress, error, start } = useModelDownload(onChanged);

  useEffect(() => {
    void api
      .models()
      .then(({ models }) => {
        setModels(models);
        const preferred =
          models.find((m) => m.installed) ?? models.find((m) => m.recommended) ?? models[0];
        setSelected(preferred?.name ?? null);
      })
      .catch(() => setListError("Could not read the model catalogue."));
  }, []);

  const current = models.find((m) => m.name === selected) ?? null;
  const shown = expanded ? models : current ? [current] : [];

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-neutral-900">
        Transcription model
      </h1>
      <p className="mt-2 max-w-md text-[14px] text-neutral-500">
        Speech recognition runs on this machine, so the model has to live here too. These are
        OpenAI's Whisper models — the same ones most local transcription apps use. One-time
        download.
      </p>

      {(listError ?? error) && (
        <div
          role="alert"
          className="mt-6 w-full max-w-md rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-left text-[13px] text-amber-900"
        >
          {listError ?? error}
        </div>
      )}

      <div className="mt-8 w-full max-w-md overflow-hidden rounded-xl border border-hairline bg-white text-left">
        <ul className="divide-y divide-hairline">
          {shown.map((model) => {
            const active = model.name === selected;
            return (
              <li key={model.name}>
                <button
                  type="button"
                  onClick={() => {
                    setSelected(model.name);
                    setExpanded(false);
                  }}
                  aria-current={active ? "true" : undefined}
                  className={`w-full px-4 py-3 text-left transition ${
                    active && expanded ? "bg-neutral-50" : "hover:bg-neutral-50"
                  }`}
                >
                  <div className="flex items-baseline gap-2">
                    <span className="text-[14px] font-medium text-neutral-900">{model.name}</span>
                    {model.recommended && (
                      <span className="rounded-full border border-emerald-200 bg-emerald-50 px-1.5 py-0.5 text-[10px] text-emerald-700">
                        recommended
                      </span>
                    )}
                    <span className="ml-auto text-[11px] text-neutral-400">
                      {size(model.bytes)} · ~{model.approx_ram_mb} MB RAM
                    </span>
                  </div>

                  <p className="mt-1 text-[12px] leading-snug text-neutral-500">
                    {model.tradeoff}
                  </p>
                  <p className="mt-0.5 text-[11px] leading-snug text-neutral-400">
                    {model.language_note}
                  </p>
                </button>
              </li>
            );
          })}
        </ul>

        <div className="flex items-center gap-2 border-t border-hairline px-4 py-2.5">
          <button
            type="button"
            onClick={() => setExpanded((open) => !open)}
            aria-expanded={expanded}
            className="flex items-center gap-1 text-[12px] text-neutral-500 transition hover:text-neutral-900"
          >
            <ChevronDown
              size={13}
              className={`transition-transform ${expanded ? "rotate-180" : ""}`}
              aria-hidden
            />
            {expanded ? "Show fewer" : `Show all ${models.length} models`}
          </button>

          <span className="ml-auto">
            {current?.installed ? (
              <span className="flex items-center gap-1 text-[12px] text-emerald-600">
                <Check size={14} aria-hidden />
                Installed
              </span>
            ) : (
              <button
                type="button"
                disabled={!current || downloading !== null}
                onClick={() => current && void start(current.name)}
                className="flex items-center gap-1.5 rounded-full border border-hairline
                           px-3 py-1.5 text-[12px] text-neutral-700 transition
                           hover:bg-neutral-50 disabled:opacity-50"
              >
                {downloading ? (
                  <>
                    <Loader2 size={13} className="animate-spin" aria-hidden />
                    Downloading
                  </>
                ) : (
                  <>
                    <Download size={13} aria-hidden />
                    Download {current?.name}
                  </>
                )}
              </button>
            )}
          </span>
        </div>

        {downloading && (
          <div className="border-t border-hairline px-4 py-3">
            <div
              role="progressbar"
              aria-valuenow={progress?.percent ?? 0}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-label={`Downloading ${downloading}`}
              className="h-1.5 w-full overflow-hidden rounded-full bg-neutral-100"
            >
              <div
                className="h-full rounded-full bg-record transition-[width] duration-300"
                style={{ width: `${progress?.percent ?? 0}%` }}
              />
            </div>

            <div className="mt-1.5 flex items-baseline justify-between text-[11px] text-neutral-500">
              <span className="font-mono tabular-nums">
                {progress
                  ? `${size(progress.downloaded_bytes)} / ${size(progress.total_bytes)}`
                  : "starting…"}
              </span>
              <span className="font-mono font-semibold tabular-nums">
                {progress?.percent ?? 0}%
              </span>
            </div>

            <p className="mt-2 text-[11px] text-neutral-400">
              The engine owns this download — it resumes where it left off if the connection
              drops, and continues if you switch away.
            </p>
          </div>
        )}
      </div>

      {satisfied && !expanded && (
        <p className="mt-3 text-[12px] text-neutral-400">
          You can add another model later in Settings.
        </p>
      )}
    </div>
  );
}
