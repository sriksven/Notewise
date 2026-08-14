import { useEffect, useState } from "react";
import { Check, Download, Loader2, Mic } from "lucide-react";

import { api, type ModelInfo } from "../../lib/api";
import { size } from "../../lib/format";
import { useModelDownload } from "../../lib/useModelDownload";

interface ModelStepProps {
  satisfied: boolean;
  /** Re-read readiness once the download lands, so the gate opens. */
  onChanged: () => Promise<void>;
}

export function ModelStep({ satisfied, onChanged }: ModelStepProps) {
  const [model, setModel] = useState<ModelInfo | null>(null);
  const [listError, setListError] = useState<string | null>(null);

  const { downloading, progress, error, start } = useModelDownload(onChanged);

  useEffect(() => {
    void api
      .models()
      .then(({ models }) => setModel(models.find((m) => m.recommended) ?? models[0] ?? null))
      .catch(() => setListError("Could not read the model catalogue."));
  }, []);

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-neutral-900">
        Transcription model
      </h1>
      <p className="mt-2 max-w-md text-[14px] text-neutral-500">
        Speech recognition runs on this machine, so the model has to live here too. This is a
        one-time download.
      </p>

      {(listError ?? error) && (
        <div
          role="alert"
          className="mt-6 w-full max-w-md rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-left text-[13px] text-amber-900"
        >
          {listError ?? error}
        </div>
      )}

      <div className="mt-8 w-full max-w-md rounded-xl border border-hairline bg-white p-4 text-left">
        <div className="flex items-center gap-3">
          <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-neutral-100">
            <Mic size={16} className="text-neutral-600" aria-hidden />
          </span>

          <div className="min-w-0 flex-1">
            <div className="text-[14px] font-medium text-neutral-900">
              {model?.name ?? "Loading…"}
            </div>
            {model && (
              // RAM is shown because it decides whether the model runs at all, which the
              // download size does not tell you.
              <div className="text-[11px] text-neutral-400">
                {size(model.bytes)} download · ~{model.approx_ram_mb} MB RAM
              </div>
            )}
          </div>

          {satisfied ? (
            <span className="flex shrink-0 items-center gap-1 text-[12px] text-emerald-600">
              <Check size={14} aria-hidden />
              Installed
            </span>
          ) : (
            <button
              type="button"
              disabled={!model || downloading !== null}
              onClick={() => model && void start(model.name)}
              className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline
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
                  Download
                </>
              )}
            </button>
          )}
        </div>

        {downloading && (
          <div className="mt-4">
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
    </div>
  );
}
