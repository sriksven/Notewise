import { useCallback, useEffect, useState } from "react";
import {
  Check,
  Cloud,
  Download,
  HardDrive,
  Loader2,
  Mic,
  Settings,
  ShieldAlert,
  ShieldCheck,
  Volume2,
} from "lucide-react";

import {
  api,
  ApiError,
  SETTINGS_BY_HAND,
  type BackendInfo,
  type ModelInfo,
  type SetupReadiness,
} from "../lib/api";
import { backendBlocker } from "../lib/backends";
import { size } from "../lib/format";
import { useModelDownload } from "../lib/useModelDownload";
import type { Mode, Theme } from "../lib/theme";
import { PermissionRow } from "../onboarding/steps/PermissionRow";
import { ApiKeyRow } from "./ApiKeyRow";
import { AppearanceSettings } from "./AppearanceSettings";
import { RoutingSettings } from "./RoutingSettings";
import { SummaryTemplateSettings } from "./SummaryTemplateSettings";
import { MemorySettings } from "./MemorySettings";
import { ToolServersSettings } from "./ToolServersSettings";
import { AssistantSettings } from "./AssistantSettings";
import { MeetingDetectionSettings } from "./MeetingDetectionSettings";
import { AudioRetentionSettings } from "./AudioRetentionSettings";
import type { Route, SettingsSection } from "../lib/router";
import { SearchIndexSettings } from "./SearchIndexSettings";
import { SpeakerSeparationSettings } from "./SpeakerSeparationSettings";
import { VoiceprintSettings } from "./VoiceprintSettings";

type PermissionKind = "microphone" | "system_audio";

/**
 * Each a page of its own. One long page with a bar that scrolled to a place on it still meant
 * reading past every other setting to find one; a page per category shows only that category.
 *
 * "Models" and "Recording" are named in the setup banner's copy, so renaming either means
 * changing `bannerProblems` too.
 */
const TABS: Array<{ id: SettingsSection; label: string }> = [
  { id: "appearance", label: "Appearance" },
  { id: "recording", label: "Recording" },
  // Split from Models: the model list alone was a two-screen page, and turning a recording into
  // text on this machine is a different question from which AI answers.
  { id: "transcription", label: "Transcription" },
  { id: "models", label: "Models" },
  // Their own pages rather than the bottom of Models and Assistant: what a summary is asked
  // for is not a model setting, and connecting an outside tool is not the assistant's memory.
  { id: "summaries", label: "Summaries" },
  { id: "assistant", label: "Assistant" },
  { id: "tools", label: "Tools" },
  { id: "search", label: "Search" },
  { id: "speakers", label: "Speakers" },
];

interface Active {
  kind: string;
  model: string;
  is_local: boolean;
}

interface SettingsViewProps {
  /** The page being shown. Kept in the address, so Back returns to the previous page. */
  section: SettingsSection;
  theme: Theme;
  onModeChange: (mode: Mode) => void;
  onAccentChange: (accent: string) => void;
  /** So a setting that is fixed elsewhere can send the user there rather than describing it. */
  onNavigate: (route: Route) => void;
  /**
   * Something here changed which model answers, or what the engine calls it.
   *
   * The top bar's model pill and its Local/Cloud badge read the window's own copy of `/health`,
   * which nothing on this screen could update — so switching to a hosted provider here left the
   * chrome saying "Local" until the window happened to refresh for some other reason.
   */
  onEngineChange: () => void;
}

export function SettingsView({
  section,
  theme,
  onModeChange,
  onAccentChange,
  onNavigate,
  onEngineChange,
}: SettingsViewProps) {
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
      onEngineChange();
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

  // The engine opens the pane: this page runs in a web view, which ignores the
  // `x-apple.systempreferences:` link these buttons used to set as `location.href`.
  const openSettings = (pane: "microphone" | "screen_recording") => {
    setError(null);
    api.openSystemSettings(pane).catch(() => setError(SETTINGS_BY_HAND));
  };

  // Permissions are read again when the window comes back — the person has just been in System
  // Settings — so a grant shows here without leaving the page. Only the setup answer: the rest of
  // the page does not change by visiting another app.
  useEffect(() => {
    const again = () => {
      api.setup().then(setReadiness).catch(() => undefined);
    };
    window.addEventListener("focus", again);
    return () => window.removeEventListener("focus", again);
  }, []);

  const permissions = readiness?.steps.permissions;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Settings size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Settings</h1>
      </header>

      {/* Outside the scrolling pane, so it cannot scroll away. Wrapping rather than scrolling
          sideways: in a narrow window a tab off the edge is a page nobody finds. Text only, so all
          nine fit on one line at the default window size. */}
      <nav
        aria-label="Settings sections"
        className="flex flex-wrap gap-1 border-b border-hairline px-8 py-2"
      >
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => onNavigate({ name: "settings", section: tab.id })}
            aria-current={section === tab.id ? "page" : undefined}
            className={`shrink-0 rounded-full px-3 py-1 text-[12.5px] transition ${
              section === tab.id
                ? "bg-overlay font-medium text-ink"
                : "text-ink-muted hover:bg-overlay hover:text-ink"
            }`}
          >
            {tab.label}
          </button>
        ))}
      </nav>

      {/* Keyed by page, so a page opens at its top rather than where the last one was scrolled. */}
      <div key={section} className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
        <div className="mx-auto max-w-2xl space-y-8">
          {(error ?? downloadError) && (
            <div
              role="alert"
              className="rounded-lg border border-warn-line bg-warn px-3 py-2 text-[13px] text-warn-text"
            >
              {error ?? downloadError}
            </div>
          )}

          {section === "appearance" && (
            <AppearanceSettings
              theme={theme}
              onModeChange={onModeChange}
              onAccentChange={onAccentChange}
            />
          )}

          {section === "recording" && (
            <>
              {/* First, because it is what the setup banner opens this page to fix. A banner that
                  points at a screen with no permissions on it is a dead end. */}
              {permissions && (
                <section>
                  <h2 className="mb-1 text-[13px] font-semibold text-ink">Permissions</h2>
                  <p className="mb-3 text-[12px] text-ink-muted">
                    What the operating system lets Notewise hear. Nothing is captured until you
                    press record.
                  </p>

                  <div className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
                    <PermissionRow
                      icon={Mic}
                      title="Microphone"
                      description="Captures your side of the conversation."
                      readiness={permissions.microphone}
                      busy={askingFor === "microphone"}
                      onEnable={() => void requestPermission("microphone")}
                      onOpenSettings={() => openSettings("microphone")}
                    />
                    <PermissionRow
                      icon={Volume2}
                      title="System audio"
                      description="Captures everyone else, straight from the meeting app."
                      readiness={permissions.system_audio}
                      busy={askingFor === "system_audio"}
                      onEnable={() => void requestPermission("system_audio")}
                      onOpenSettings={() => openSettings("screen_recording")}
                    />
                  </div>
                </section>
              )}

              {/* This decides whether a meeting gets captured at all; every other page is about
                  what happens to one that was. */}
              <MeetingDetectionSettings onNavigate={onNavigate} />

              {/* Directly after it: that setting decides whether audio is captured, and this one
                  decides whether it survives being turned into text. */}
              <AudioRetentionSettings />
            </>
          )}

          {section === "models" && (
            <>
              {/* Where audio goes is the product's central claim, so it leads the models. */}
              <section>
                <h2 className="mb-1 text-[13px] font-semibold text-ink">AI backend</h2>
                <p className="mb-3 text-[12px] text-ink-muted">
                  Runs summaries, chat and suggested questions. Click one to switch — it takes
                  effect immediately, no restart. A key you paste goes to your OS keychain and is
                  never shown again; an environment variable still works if you prefer one.
                </p>

                {active && (
                  <div
                    className={`mb-3 flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px] ${
                      active.is_local
                        ? "border-ok-line bg-ok text-ok-text"
                        : "border-warn-line bg-warn text-warn-text"
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
                    // Blocked only while something is genuinely missing. A provider whose key is
                    // saved is as usable as a local one, and greying it out was the reason a key
                    // could be added and then appear not to work.
                    const blocked = backendBlocker(backend) !== null;

                    return (
                      <li key={backend.kind}>
                        <button
                          type="button"
                          disabled={blocked || switching !== null}
                          onClick={() => void switchTo(backend.kind)}
                          aria-current={isActive ? "true" : undefined}
                          className={`flex w-full items-center gap-3 px-3 py-2 text-left transition
                                      disabled:cursor-not-allowed ${
                                        isActive ? "bg-overlay" : "bg-surface hover:bg-overlay"
                                      }`}
                        >
                          {backend.is_local ? (
                            <HardDrive size={14} className="shrink-0 text-ok-text" aria-hidden />
                          ) : (
                            <Cloud size={14} className="shrink-0 text-ink-faint" aria-hidden />
                          )}

                          <span
                            className={`flex-1 text-[13px] ${
                              blocked ? "text-ink-faint" : "text-ink"
                            }`}
                          >
                            {backend.label}
                          </span>

                          {switching === backend.kind && (
                            <Loader2 size={13} className="animate-spin text-ink-faint" aria-hidden />
                          )}
                          {isActive && <Check size={14} className="text-ok-text" aria-hidden />}

                          {backend.requires_api_key && !backend.has_key && (
                            <span className="rounded-full border border-hairline bg-overlay px-1.5 py-0.5 text-[10px] text-ink-muted">
                              needs key
                            </span>
                          )}
                          {backend.requires_api_key && backend.has_key && (
                            <span className="rounded-full border border-ok-line bg-ok px-1.5 py-0.5 text-[10px] text-ok-text">
                              key saved
                            </span>
                          )}
                          {backend.requires_endpoint && (
                            <span className="rounded-full border border-hairline bg-overlay px-1.5 py-0.5 text-[10px] text-ink-muted">
                              needs URL
                            </span>
                          )}
                        </button>

                        {/* The models this backend actually holds, listed under it while it is the
                            active one. Choosing a provider was never enough: the tag has to match
                            what was pulled, exactly. */}
                        {backend.requires_api_key && (
                          // Saving a key also switches to that provider, and removing one can leave
                          // the engine on another — either way the chrome has to hear about it.
                          <ApiKeyRow
                            backend={backend}
                            onChanged={() => {
                              onEngineChange();
                              void load();
                            }}
                          />
                        )}

                        {isActive && available && (
                          <div className="border-t border-hairline bg-overlay px-3 py-2">
                            {available.models.length === 0 ? (
                              <p className="text-[12px] text-ink-muted">
                                {available.reason ??
                                  `No models found. Pull one with \`ollama pull llama3.1\`.`}
                              </p>
                            ) : (
                              <>
                                <p className="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
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
                                                      ? "border-ok-line bg-ok text-ok-text"
                                                      : "border-hairline bg-surface text-ink hover:bg-overlay"
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

              <RoutingSettings onChanged={onEngineChange} />
            </>
          )}

          {/* Its own page: the model list is long, and it is a different question from which AI
              answers — it is what turns a recording into text, on this machine. */}
          {section === "transcription" && (
            <section>
              <h2 className="mb-1 text-[13px] font-semibold text-ink">Transcription models</h2>
              <p className="mb-3 text-[12px] text-ink-muted">
                Whisper, from OpenAI — the same models MacWhisper, whisper.cpp and most local
                transcription tools use. Bigger is more accurate and slower. A name ending in{" "}
                <code className="rounded bg-overlay px-1">.en</code> is English-only. Stored
                in{" "}
                <code className="rounded bg-overlay px-1">{directory}</code>
              </p>

              <ul className="divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
                {models.map((model) => (
                  <li key={model.name} className="flex items-start gap-3 bg-surface px-3 py-2.5">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="text-[13px] font-medium text-ink">{model.name}</span>
                        {model.recommended && (
                          <span className="rounded-full border border-ok-line bg-ok px-1.5 py-0.5 text-[10px] text-ok-text">
                            recommended
                          </span>
                        )}
                      </div>

                      <p className="mt-0.5 text-[12px] leading-snug text-ink-muted">
                        {model.tradeoff}
                      </p>
                      <p className="mt-0.5 text-[11px] leading-snug text-ink-faint">
                        {model.language_note}
                      </p>
                      {/* RAM is shown because it is the number that decides whether a model is
                          usable at all — a 3GB model on an 8GB machine will not run. */}
                      <span className="mt-1 block text-[11px] text-ink-faint">
                        {size(model.bytes)} download · ~{model.approx_ram_mb} MB RAM
                      </span>
                    </div>

                    {model.installed ? (
                      <span className="flex shrink-0 items-center gap-1 pt-0.5 text-[12px] text-ok-text">
                        <Check size={14} aria-hidden />
                        Installed
                      </span>
                    ) : (
                      <button
                        type="button"
                        onClick={() => void start(model.name)}
                        disabled={downloading !== null}
                        className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline
                                   px-2.5 py-1 text-[12px] text-ink transition
                                   hover:bg-overlay disabled:opacity-50"
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
                  <div className="mb-1 flex items-baseline justify-between text-[11px] text-ink-muted">
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
                    className="h-1.5 w-full overflow-hidden rounded-full bg-overlay"
                  >
                    <div
                      className="h-full rounded-full bg-record transition-[width] duration-300"
                      style={{ width: `${progress?.percent ?? 0}%` }}
                    />
                  </div>
                  <p className="mt-1.5 text-[11px] text-ink-faint">
                    The engine owns this download — it continues if you switch views, and
                    resumes where it left off if the connection drops.
                  </p>
                </div>
              )}
            </section>
          )}

          {section === "summaries" && <SummaryTemplateSettings />}

          {section === "assistant" && (
            <>
              <MemorySettings />

              {/* After memory: this is a way for Notewise to act outside itself, so it reads after
                  what only changes what it knows. */}
              <AssistantSettings />
            </>
          )}

          {section === "tools" && <ToolServersSettings />}

          {section === "search" && <SearchIndexSettings />}

          {section === "speakers" && (
            <>
              {/* Above voiceprints deliberately: separating voices is the anonymous, local step,
                  and recognising them across meetings is the one that identifies a person. Reading
                  them in that order is reading them from least to most consequential. */}
              <SpeakerSeparationSettings />

              <VoiceprintSettings />
            </>
          )}
        </div>
      </div>
    </div>
  );
}
