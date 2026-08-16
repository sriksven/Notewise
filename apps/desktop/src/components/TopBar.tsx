import { useEffect, useRef, useState } from "react";
import { Check, Globe, Mic, Settings2, ShieldAlert, ShieldCheck } from "lucide-react";

import { api, type BackendInfo, type DeviceInfo, type Health, type LanguageOption } from "../lib/api";

interface Props {
  health: Health | null;
  /** Locked while recording: changing input or language mid-meeting cannot take effect. */
  isRecording: boolean;
  device: string | null;
  onDeviceChange: (device: string | null) => void;
  language: string | null;
  onLanguageChange: (language: string | null) => void;
  onBackendChange: () => void;
  onError: (message: string) => void;
}

/**
 * A pill with a dropdown.
 *
 * Closes on outside click and on Escape. A menu that only closes by re-clicking its own trigger
 * is a menu users end up dismissing by clicking something they did not mean to press.
 */
function Pill({
  icon,
  label,
  disabled,
  disabledReason,
  children,
}: {
  icon: React.ReactNode;
  label: string;
  disabled?: boolean;
  disabledReason?: string;
  children: (close: () => void) => React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;

    const onPointerDown = (event: MouseEvent) => {
      if (!container.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };

    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div className="relative" ref={container}>
      <button
        type="button"
        onClick={() => !disabled && setOpen((o) => !o)}
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={open}
        title={disabled ? disabledReason : label}
        className="pill disabled:cursor-not-allowed disabled:opacity-50"
      >
        {icon}
        {label}
      </button>

      {open && (
        <div
          role="menu"
          className="absolute left-1/2 top-full z-20 mt-1.5 max-h-80 w-60 -translate-x-1/2
                     overflow-y-auto rounded-xl border border-hairline bg-surface py-1 shadow-dock"
        >
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

function Item({
  selected,
  onClick,
  title,
  subtitle,
  disabled,
}: {
  selected: boolean;
  onClick: () => void;
  title: string;
  subtitle?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onClick}
      className="flex w-full items-start gap-2 px-3 py-1.5 text-left text-[13px]
                 text-ink transition hover:bg-overlay
                 disabled:cursor-not-allowed disabled:text-ink-faint"
    >
      <span className="mt-0.5 w-3.5 shrink-0">
        {selected && <Check size={13} className="text-record" aria-hidden />}
      </span>
      <span className="min-w-0">
        <span className="block truncate">{title}</span>
        {subtitle && (
          <span className="block truncate text-[11px] text-ink-faint">{subtitle}</span>
        )}
      </span>
    </button>
  );
}

/**
 * The three configuration pills, plus the panel toggle.
 *
 * These sit above the content rather than in a settings screen because model, input device, and
 * language are decisions a user revisits per meeting — burying them costs more than the header
 * space. Each one is backed by a real endpoint; a pill that only looked like a control would be
 * worse than no pill at all.
 */
export function TopBar({
  health,
  isRecording,
  device,
  onDeviceChange,
  language,
  onLanguageChange,
  onBackendChange,
  onError,
}: Props) {
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [languages, setLanguages] = useState<LanguageOption[]>([]);
  /** Exact model tags the active local backend holds. */
  const [installed, setInstalled] = useState<string[]>([]);
  /** Which backend is running. `health` carries the model name but not the kind. */
  const [activeKind, setActiveKind] = useState<string | null>(null);

  // Devices and languages are loaded once — neither changes while the app is open. The backend
  // list is re-read whenever the active model changes, so switching updates the tick.
  useEffect(() => {
    void api
      .backends()
      .then((r) => {
        setBackends(r.backends);
        setActiveKind(r.active.kind);
      })
      .catch(() => setBackends([]));
  }, [health?.ai_model]);

  useEffect(() => {
    void api.devices().then((r) => setDevices(r.devices)).catch(() => setDevices([]));
    void api.languages().then((r) => setLanguages(r.languages)).catch(() => setLanguages([]));
  }, []);

  // Which models the running backend actually has. Picking a provider was never enough: the
  // engine's default tag is a guess, and a machine holding `llama3.1:8b` answers `llama3.1`
  // with a 404 that nothing in this window could previously correct.
  useEffect(() => {
    const listable = backends.find((b) => b.kind === activeKind)?.lists_models;
    if (!activeKind || !listable) {
      setInstalled([]);
      return;
    }

    let cancelled = false;
    void api
      .backendModels(activeKind)
      .then((r) => !cancelled && setInstalled(r.models))
      .catch(() => !cancelled && setInstalled([]));

    return () => {
      cancelled = true;
    };
  }, [activeKind, backends]);

  const switchTo = async (backend: BackendInfo, close: () => void, model?: string) => {
    close();
    try {
      await api.switchBackend(backend.kind, model);
      onBackendChange();
    } catch (e) {
      onError(
        e instanceof Error ? e.message : `Could not switch to ${model ?? backend.label}.`,
      );
    }
  };

  // Named only once chosen. A pill reading "Detect" on its own says nothing about what it
  // controls; "Language" does, and the menu explains the default.
  const languageLabel = language
    ? (languages.find((l) => l.code === language)?.label ?? language)
    : "Language";

  return (
    <header className="chrome relative flex h-14 shrink-0 items-center justify-center border-b border-hairline px-3">
      <div className="flex items-center gap-2">
        <Pill
          icon={<Settings2 size={14} aria-hidden />}
          label={health?.ai_model ?? "Model"}
        >
          {(close) => (
            <>
              <p className="px-3 pb-1 pt-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
                AI backend
              </p>
              {backends.length === 0 && (
                <p className="px-3 py-2 text-[12px] text-ink-faint">
                  Could not reach the engine.
                </p>
              )}
              {backends.map((backend) => {
                // A cloud backend with no key on the engine cannot be selected. Showing it
                // greyed with the reason beats hiding it and leaving the user wondering why
                // their provider is missing.
                const blocked = backend.requires_api_key || backend.requires_endpoint;
                return (
                  <Item
                    key={backend.kind}
                    selected={backend.kind === activeKind}
                    disabled={blocked}
                    onClick={() => void switchTo(backend, close)}
                    title={backend.label}
                    subtitle={
                      backend.requires_api_key
                        ? "needs an API key in the engine's environment"
                        : backend.requires_endpoint
                          ? "needs an endpoint URL — set it in Settings"
                          : backend.is_local
                            ? "runs on this machine"
                            : "sends transcripts to the provider"
                    }
                  />
                );
              })}

              {/* The exact tags this machine holds. Without these the pill picks a provider
                  and leaves the model as whatever the engine guessed, which is how a working
                  Ollama ends up returning "model not found" for everything. */}
              {installed.length > 0 && (
                <>
                  <div className="my-1 border-t border-hairline" />
                  <p className="px-3 pb-1 pt-1 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
                    Installed models
                  </p>
                  {installed.map((model) => {
                    const backend = backends.find((b) => b.kind === activeKind);
                    return (
                      <Item
                        key={model}
                        selected={model === health?.ai_model}
                        onClick={() => backend && void switchTo(backend, close, model)}
                        title={model}
                      />
                    );
                  })}
                </>
              )}
            </>
          )}
        </Pill>

        <Pill
          icon={<Mic size={14} aria-hidden />}
          label={device ? device.split(" ")[0] : "Devices"}
          disabled={isRecording}
          disabledReason="Stop recording to change the input device"
        >
          {(close) => (
            <>
              <p className="px-3 pb-1 pt-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
                Input device
              </p>
              <Item
                selected={device === null}
                onClick={() => {
                  onDeviceChange(null);
                  close();
                }}
                title="System default"
                subtitle="follows whatever macOS is using"
              />
              {devices.map((d) => (
                <Item
                  key={d.name}
                  selected={device === d.name}
                  onClick={() => {
                    onDeviceChange(d.name);
                    close();
                  }}
                  title={d.name}
                  subtitle={`${(d.sample_rate / 1000).toFixed(1)} kHz · ${d.channels} ch${
                    d.is_default ? " · default" : ""
                  }`}
                />
              ))}
              {devices.length === 0 && (
                <p className="px-3 py-2 text-[12px] text-ink-faint">
                  No input devices found.
                </p>
              )}
            </>
          )}
        </Pill>

        <Pill
          icon={<Globe size={14} aria-hidden />}
          label={languageLabel}
          disabled={isRecording}
          disabledReason="Stop recording to change the language"
        >
          {(close) => (
            <>
              <p className="px-3 pb-1 pt-1.5 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
                Spoken language
              </p>
              <Item
                selected={language === null}
                onClick={() => {
                  onLanguageChange(null);
                  close();
                }}
                title="Detect"
                subtitle="let the model work it out"
              />
              {languages.map((l) => (
                <Item
                  key={l.code}
                  selected={language === l.code}
                  onClick={() => {
                    onLanguageChange(l.code);
                    close();
                  }}
                  title={l.label}
                  subtitle={l.code}
                />
              ))}
              <p className="px-3 pb-1.5 pt-1 text-[11px] leading-snug text-ink-faint">
                English-only models ignore this.
              </p>
            </>
          )}
        </Pill>
      </div>

      {/* Where a user's audio goes is the product's central claim, so it is stated
          in the chrome rather than left in a settings screen to be trusted. */}
      {health && (
        <div
          className="absolute right-3 flex items-center gap-1.5 text-[12px] text-ink-muted"
          title={
            health.ai_local
              ? "Transcripts are processed on this machine"
              : `Transcripts are sent to ${health.ai_model}`
          }
        >
          {health.ai_local ? (
            <ShieldCheck size={14} className="text-ok-text" aria-hidden />
          ) : (
            <ShieldAlert size={14} className="text-warn-text" aria-hidden />
          )}
          <span className="hidden sm:inline">{health.ai_local ? "Local" : "Cloud"}</span>
        </div>
      )}
    </header>
  );
}
