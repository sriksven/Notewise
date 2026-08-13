import { useEffect, useState } from "react";
import { Check, Cloud, Download, HardDrive, Loader2, ShieldAlert, ShieldCheck } from "lucide-react";

import { api, ApiError, type DownloadState, type BackendInfo, type ModelInfo } from "../lib/api";

/** Bytes as GB/MB. Model sizes span 77 MB to 3 GB, so one unit does not serve both. */
function size(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}

export function SettingsView() {
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [active, setActive] = useState<{ model: string; is_local: boolean } | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [directory, setDirectory] = useState("");
  const [downloading, setDownloading] = useState<string | null>(null);
  const [progress, setProgress] = useState<DownloadState | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      const [b, m] = await Promise.all([api.backends(), api.models()]);
      setBackends(b.backends);
      setActive(b.active);
      setModels(m.models);
      setDirectory(m.directory);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load settings.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const download = async (name: string) => {
    setDownloading(name);
    setProgress(null);
    setError(null);

    try {
      const started = await api.downloadModel(name);

      // Already on disk: the POST answers `done` and there is nothing to stream.
      if (started.status === "done") {
        setDownloading(null);
        await load();
        return;
      }

      setProgress(started);
      api.watchDownload(
        name,
        setProgress,
        async () => {
          setDownloading(null);
          setProgress(null);
          await load();
        },
        (message) => {
          setError(message);
          setDownloading(null);
          setProgress(null);
        },
      );
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Download failed.");
      setDownloading(null);
    }
  };

  // Recover a download already running when this view mounted — the engine owns it, so
  // switching away from Settings and back must not lose the progress bar.
  useEffect(() => {
    let cancel: (() => void) | undefined;

    void api.downloads().then((states) => {
      const running = states.find((s) => s.status === "downloading");
      if (!running) return;

      setDownloading(running.model);
      setProgress(running);
      cancel = api.watchDownload(
        running.model,
        setProgress,
        async () => {
          setDownloading(null);
          setProgress(null);
          await load();
        },
        (message) => {
          setError(message);
          setDownloading(null);
          setProgress(null);
        },
      );
    });

    return () => cancel?.();
  }, []);

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
      <div className="mx-auto max-w-2xl space-y-8">
        <h1 className="text-[20px] font-semibold tracking-tight">Settings</h1>

        {error && (
          <div role="alert" className="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-[13px] text-amber-900">
            {error}
          </div>
        )}

        {/* Where audio goes is the product's central claim, so it leads. */}
        <section>
          <h2 className="mb-1 text-[13px] font-semibold text-neutral-900">AI backend</h2>
          <p className="mb-3 text-[12px] text-neutral-500">
            Set with the <code className="rounded bg-neutral-100 px-1">NOTEWISE_BACKEND</code>{" "}
            environment variable and a provider API key. Restart the engine to change it.
          </p>

          {active && (
            <div
              className={`mb-3 flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px] ${
                active.is_local
                  ? "border-emerald-200 bg-emerald-50 text-emerald-900"
                  : "border-amber-200 bg-amber-50 text-amber-900"
              }`}
            >
              {active.is_local ? <ShieldCheck size={15} aria-hidden /> : <ShieldAlert size={15} aria-hidden />}
              <span>
                <strong>{active.model}</strong> —{" "}
                {active.is_local
                  ? "transcripts stay on this machine"
                  : "transcripts are sent to the provider"}
              </span>
            </div>
          )}

          <ul className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
            {backends.map((backend) => (
              <li key={backend.kind} className="flex items-center gap-3 bg-white px-3 py-2">
                {backend.is_local ? (
                  <HardDrive size={14} className="shrink-0 text-emerald-600" aria-hidden />
                ) : (
                  <Cloud size={14} className="shrink-0 text-neutral-400" aria-hidden />
                )}

                <span className="flex-1 text-[13px] text-neutral-800">{backend.label}</span>

                <code className="text-[11px] text-neutral-400">{backend.kind}</code>

                {backend.requires_api_key && (
                  <span className="rounded-full border border-neutral-200 bg-neutral-50 px-1.5 py-0.5 text-[10px] text-neutral-500">
                    needs key
                  </span>
                )}
                {backend.requires_endpoint && (
                  <span className="rounded-full border border-neutral-200 bg-neutral-50 px-1.5 py-0.5 text-[10px] text-neutral-500">
                    needs URL
                  </span>
                )}
              </li>
            ))}
          </ul>
        </section>

        <section>
          <h2 className="mb-1 text-[13px] font-semibold text-neutral-900">
            Transcription models
          </h2>
          <p className="mb-3 text-[12px] text-neutral-500">
            Downloaded to <code className="rounded bg-neutral-100 px-1">{directory}</code>
          </p>

          <ul className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
            {models.map((model) => (
              <li key={model.name} className="flex items-center gap-3 bg-white px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-neutral-900">{model.name}</span>
                    {model.recommended && (
                      <span className="rounded-full border border-emerald-200 bg-emerald-50 px-1.5 py-0.5 text-[10px] text-emerald-700">
                        recommended
                      </span>
                    )}
                    {model.multilingual && (
                      <span className="text-[10px] text-neutral-400">multilingual</span>
                    )}
                  </div>
                  {/* RAM is shown because it is the number that decides whether a model is
                      usable at all — a 3GB model on an 8GB machine will not run. */}
                  <span className="text-[11px] text-neutral-400">
                    {size(model.bytes)} download · ~{model.approx_ram_mb} MB RAM
                  </span>
                </div>

                {model.installed ? (
                  <span className="flex shrink-0 items-center gap-1 text-[12px] text-emerald-600">
                    <Check size={14} aria-hidden />
                    Installed
                  </span>
                ) : (
                  <button
                    type="button"
                    onClick={() => download(model.name)}
                    disabled={downloading !== null}
                    className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline
                               px-2.5 py-1 text-[12px] text-neutral-700 transition
                               hover:bg-neutral-50 disabled:opacity-50"
                  >
                    {downloading === model.name ? (
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
              </li>
            ))}
          </ul>

          {downloading && (
            <div className="mt-3">
              <div className="mb-1 flex items-baseline justify-between text-[11px] text-neutral-500">
                <span>Downloading {downloading}</span>
                <span className="font-mono tabular-nums">
                  {progress
                    ? `${size(progress.downloaded_bytes)} / ${size(
                        progress.total_bytes,
                      )} · ${progress.percent}%`
                    : "starting…"}
                </span>
              </div>
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
              <p className="mt-1.5 text-[11px] text-neutral-400">
                The engine owns this download — it continues if you switch views, and resumes
                where it left off if the connection drops.
              </p>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
