import { useCallback, useEffect, useState } from "react";
import {
  Check,
  Cloud,
  Download,
  HardDrive,
  Loader2,
  Mic,
  ShieldAlert,
  ShieldCheck,
  Volume2,
} from "lucide-react";

import { api, ApiError, type BackendInfo, type ModelInfo, type SetupReadiness } from "../lib/api";
import { size } from "../lib/format";
import { useModelDownload } from "../lib/useModelDownload";
import { PermissionRow } from "../onboarding/steps/PermissionRow";

/** macOS deep link to Privacy & Security. Harmless elsewhere — the OS ignores it. */
const PRIVACY_SETTINGS = "x-apple.systempreferences:com.apple.preference.security";

type PermissionKind = "microphone" | "system_audio";

interface Active {
  kind: string;
  model: string;
  is_local: boolean;
}

export function SettingsView() {
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [active, setActive] = useState<Active | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [directory, setDirectory] = useState("");
  const [readiness, setReadiness] = useState<SetupReadiness | null>(null);
  const [error, setError] = useState<string | null>(null);

  /** Models the active local backend actually holds, and why not when it holds none. */
  const [available, setAvailable] = useState<{ models: string[]; reason: string | null } | null>(
    null,
  );
  const [switching, setSwitching] = useState<string | null>(null);
  const [askingFor, setAskingFor] = useState<PermissionKind | null>(null);

  const load = useCallback(async () => {
    try {
      const [b, m, s] = await Promise.all([api.backends(), api.models(), api.setup()]);
      setBackends(b.backends);
      setActive(b.active);
      setModels(m.models);
      setDirectory(m.directory);
      setReadiness(s);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load settings.");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Ask the running backend what it can actually run. The engine's default model id is only a
  // guess — a machine with `llama3.1:8b` pulled answers `llama3.1` with a 404, and this list is
  // the only way to see that from inside the app.
  useEffect(() => {
    const kind = active?.kind;
    const listable = backends.find((b) => b.kind === kind)?.lists_models;
    if (!kind || !listable) {
      setAvailable(null);
      return;
    }

    let cancelled = false;
    void api
      .backendModels(kind)
      .then((r) => !cancelled && setAvailable({ models: r.models, reason: r.reason }))
      .catch(() => !cancelled && setAvailable(null));

    return () => {
      cancelled = true;
    };
  }, [active?.kind, backends]);

  const { downloading, progress, error: downloadError, start } = useModelDownload(load);

  const switchTo = async (kind: string, model?: string) => {
    setSwitching(model ?? kind);
    setError(null);
    try {
      await api.switchBackend(kind, model);
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not switch backend.");
    } finally {
      setSwitching(null);
    }
  };

  const requestPermission = async (kind: PermissionKind) => {
    setAskingFor(kind);
    setError(null);
    try {
      await api.requestPermission(kind);
      await load();
    } catch {
      setError("Could not ask the system for that permission.");
    } finally {
      setAskingFor(null);
    }
  };

  const permissions = readiness?.steps.permissions;

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
      <div className="mx-auto max-w-2xl space-y-8">
        <h1 className="text-[20px] font-semibold tracking-tight">Settings</h1>

        {(error ?? downloadError) && (
          <div
            role="alert"
            className="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-[13px] text-amber-900"
          >
            {error ?? downloadError}
          </div>
        )}

        {/* First, because it is what the setup banner sends people here to fix. A banner that
            points at a screen with no permissions on it is a dead end. */}
        {permissions && (
          <section>
            <h2 className="mb-1 text-[13px] font-semibold text-neutral-900">Permissions</h2>
            <p className="mb-3 text-[12px] text-neutral-500">
              What the operating system lets Notewise hear. Nothing is captured until you press
              record.
            </p>

            <div className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
              <PermissionRow
                icon={Mic}
                title="Microphone"
                description="Captures your side of the conversation."
                readiness={permissions.microphone}
                busy={askingFor === "microphone"}
                onEnable={() => void requestPermission("microphone")}
                onOpenSettings={() => {
                  window.location.href = PRIVACY_SETTINGS;
                }}
              />
              <PermissionRow
                icon={Volume2}
                title="System audio"
                description="Captures everyone else, straight from the meeting app."
                readiness={permissions.system_audio}
                busy={askingFor === "system_audio"}
                onEnable={() => void requestPermission("system_audio")}
                onOpenSettings={() => {
                  window.location.href = PRIVACY_SETTINGS;
                }}
              />
            </div>
          </section>
        )}

        {/* Where audio goes is the product's central claim, so it leads the rest. */}
        <section>
          <h2 className="mb-1 text-[13px] font-semibold text-neutral-900">AI backend</h2>
          <p className="mb-3 text-[12px] text-neutral-500">
            Runs summaries, chat and suggested questions. Click one to switch — it takes effect
            immediately, no restart. API keys are read from the engine's own environment and are
            never sent from this window.
          </p>

          {active && (
            <div
              className={`mb-3 flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px] ${
                active.is_local
                  ? "border-emerald-200 bg-emerald-50 text-emerald-900"
                  : "border-amber-200 bg-amber-50 text-amber-900"
              }`}
            >
              {active.is_local ? (
                <ShieldCheck size={15} aria-hidden />
              ) : (
                <ShieldAlert size={15} aria-hidden />
              )}
              <span>
                <strong>{active.model}</strong> —{" "}
                {active.is_local
                  ? "transcripts stay on this machine"
                  : "transcripts are sent to the provider"}
              </span>
            </div>
          )}

          <ul className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
            {backends.map((backend) => {
              const isActive = backend.kind === active?.kind;
              const blocked = backend.requires_api_key || backend.requires_endpoint;

              return (
                <li key={backend.kind}>
                  <button
                    type="button"
                    disabled={blocked || switching !== null}
                    onClick={() => void switchTo(backend.kind)}
                    aria-current={isActive ? "true" : undefined}
                    className={`flex w-full items-center gap-3 px-3 py-2 text-left transition
                                disabled:cursor-not-allowed ${
                                  isActive ? "bg-neutral-50" : "bg-white hover:bg-neutral-50"
                                }`}
                  >
                    {backend.is_local ? (
                      <HardDrive size={14} className="shrink-0 text-emerald-600" aria-hidden />
                    ) : (
                      <Cloud size={14} className="shrink-0 text-neutral-400" aria-hidden />
                    )}

                    <span
                      className={`flex-1 text-[13px] ${
                        blocked ? "text-neutral-400" : "text-neutral-800"
                      }`}
                    >
                      {backend.label}
                    </span>

                    {switching === backend.kind && (
                      <Loader2 size={13} className="animate-spin text-neutral-400" aria-hidden />
                    )}
                    {isActive && <Check size={14} className="text-emerald-600" aria-hidden />}

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
                  </button>

                  {/* The models this backend actually holds, listed under it while it is the
                      active one. Choosing a provider was never enough: the tag has to match
                      what was pulled, exactly. */}
                  {isActive && available && (
                    <div className="border-t border-hairline bg-neutral-50/60 px-3 py-2">
                      {available.models.length === 0 ? (
                        <p className="text-[12px] text-neutral-500">
                          {available.reason ??
                            `No models found. Pull one with \`ollama pull llama3.1\`.`}
                        </p>
                      ) : (
                        <>
                          <p className="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-neutral-400">
                            Installed models
                          </p>
                          <div className="flex flex-wrap gap-1.5">
                            {available.models.map((model) => (
                              <button
                                key={model}
                                type="button"
                                disabled={switching !== null}
                                onClick={() => void switchTo(backend.kind, model)}
                                className={`rounded-full border px-2.5 py-1 text-[12px] transition
                                            disabled:opacity-50 ${
                                              model === active?.model
                                                ? "border-emerald-300 bg-emerald-50 text-emerald-800"
                                                : "border-hairline bg-white text-neutral-700 hover:bg-neutral-50"
                                            }`}
                              >
                                {switching === model ? "Switching…" : model}
                              </button>
                            ))}
                          </div>
                        </>
                      )}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </section>

        <section>
          <h2 className="mb-1 text-[13px] font-semibold text-neutral-900">Transcription models</h2>
          <p className="mb-3 text-[12px] text-neutral-500">
            Whisper, from OpenAI — the same models MacWhisper, whisper.cpp and most local
            transcription tools use. Bigger is more accurate and slower. A name ending in{" "}
            <code className="rounded bg-neutral-100 px-1">.en</code> is English-only. Stored in{" "}
            <code className="rounded bg-neutral-100 px-1">{directory}</code>
          </p>

          <ul className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
            {models.map((model) => (
              <li key={model.name} className="flex items-start gap-3 bg-white px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-neutral-900">{model.name}</span>
                    {model.recommended && (
                      <span className="rounded-full border border-emerald-200 bg-emerald-50 px-1.5 py-0.5 text-[10px] text-emerald-700">
                        recommended
                      </span>
                    )}
                  </div>

                  <p className="mt-0.5 text-[12px] leading-snug text-neutral-500">
                    {model.tradeoff}
                  </p>
                  <p className="mt-0.5 text-[11px] leading-snug text-neutral-400">
                    {model.language_note}
                  </p>
                  {/* RAM is shown because it is the number that decides whether a model is
                      usable at all — a 3GB model on an 8GB machine will not run. */}
                  <span className="mt-1 block text-[11px] text-neutral-400">
                    {size(model.bytes)} download · ~{model.approx_ram_mb} MB RAM
                  </span>
                </div>

                {model.installed ? (
                  <span className="flex shrink-0 items-center gap-1 pt-0.5 text-[12px] text-emerald-600">
                    <Check size={14} aria-hidden />
                    Installed
                  </span>
                ) : (
                  <button
                    type="button"
                    onClick={() => void start(model.name)}
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
