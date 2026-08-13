import { useEffect, useState } from "react";
import { Check, Cloud, Download, HardDrive, Loader2, ShieldAlert, ShieldCheck } from "lucide-react";

import { api, ApiError, type BackendInfo, type ModelInfo } from "../lib/api";

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
    setError(null);
    try {
      await api.downloadModel(name);
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Download failed.");
    } finally {
      setDownloading(null);
    }
  };

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
            // Honest about the limitation rather than faking a progress bar: the endpoint
            // returns only on completion, so there is no percentage to report.
            <p className="mt-2 text-[11px] text-neutral-400">
              Downloading {downloading} — no progress is reported until it finishes.
            </p>
          )}
        </section>
      </div>
    </div>
  );
}
