import { useEffect, useState } from "react";
import { AlertTriangle, Circle, Mic, MicOff, Square, Upload } from "lucide-react";

import { duration } from "../lib/format";
import type { DeviceInfo, Health, Segment } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  health: Health | null;
  isRecording: boolean;
  /**
   * A meeting is open but nothing is being captured into it.
   *
   * Only reachable on an engine that cannot record. Kept distinct from `isRecording` so the
   * page never claims a microphone is live on a build that has no capture compiled in.
   */
  openMeeting: boolean;
  startedAt: number | null;
  busy: boolean;
  /** The input the engine is actually capturing from, once it is running. */
  liveDevice: string | null;
  /** The input the user picked for the next recording, or null for the system default. */
  preferredDevice: string | null;
  onDeviceChange: (device: string | null) => void;
  language: string | null;
  /** Transcript of the meeting being recorded, so this page shows words arriving. */
  segments: Segment[];
  onToggle: () => void;
  onImport: () => void;
  onNavigate: (route: Route) => void;
  /** Where the live meeting is, so the page can offer to open it. */
  recordingId: string | null;
  listDevices: () => Promise<{ devices: DeviceInfo[]; available: boolean; error?: string }>;
}

/**
 * The record page.
 *
 * One screen whose whole job is the moment before and during a meeting: pick the input, press
 * the button, watch the words arrive. It exists separately from the meeting page because those
 * are different tasks — the meeting page is for reading a transcript afterwards, and burying
 * the start button inside it means hunting for it with a call already ringing.
 *
 * The device picker is loaded on mount and on every change of recording state, not once ever.
 * Headphones get plugged in between launches, and a stale list is how someone records a
 * meeting on the wrong microphone.
 */
export function RecordView({
  health,
  isRecording,
  openMeeting,
  startedAt,
  busy,
  liveDevice,
  preferredDevice,
  onDeviceChange,
  language,
  segments,
  onToggle,
  onImport,
  onNavigate,
  recordingId,
  listDevices,
}: Props) {
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [deviceError, setDeviceError] = useState<string | null>(null);
  const [, tick] = useState(0);

  const canRecord = health?.can_record ?? false;

  useEffect(() => {
    let cancelled = false;
    void listDevices()
      .then((result) => {
        if (cancelled) return;
        setDevices(result.devices);
        setDeviceError(result.error ?? null);
      })
      .catch(() => {
        if (!cancelled) setDeviceError("Could not read the input list.");
      });
    return () => {
      cancelled = true;
    };
  }, [listDevices, isRecording]);

  // The clock only ticks while something is running; an idle page does no work.
  useEffect(() => {
    if (!isRecording) return;
    const id = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [isRecording]);

  const elapsed = startedAt === null ? 0 : Date.now() - startedAt;
  const recent = segments.slice(-6);

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center px-8 py-8">
      <div className="w-full max-w-lg">
        {!canRecord && (
          <div
            role="status"
            className="mb-6 flex items-start gap-2 rounded-xl border border-warn-line bg-warn px-4 py-3
                       text-[12.5px] leading-relaxed text-warn-text"
          >
            <AlertTriangle size={15} className="mt-0.5 shrink-0" aria-hidden />
            <span>
              This build cannot capture audio. Pressing record still creates a meeting you can
              import a transcript into.{" "}
              <button
                type="button"
                onClick={() => onNavigate({ name: "help", section: "docs" })}
                className="underline underline-offset-2"
              >
                What this means
              </button>
            </span>
          </div>
        )}

        <div className="flex flex-col items-center">
          {/* The button is the page. Large, centred, and labelled — starting a recording opens
              a microphone, and that should never be something a user does by accident or is
              unsure they did. */}
          <button
            type="button"
            onClick={onToggle}
            disabled={busy}
            aria-pressed={isRecording}
            className={`flex h-24 w-24 flex-col items-center justify-center gap-1 rounded-full
                        text-white transition disabled:opacity-60 ${
                          isRecording
                            ? "recording-pulse bg-record hover:bg-record-hover"
                            : openMeeting || !canRecord
                              ? "bg-ink-faint hover:bg-ink-muted"
                              : "bg-record hover:bg-record-hover"
                        }`}
          >
            {isRecording || openMeeting ? (
              <Square size={26} fill="currentColor" aria-hidden />
            ) : canRecord ? (
              <Mic size={30} strokeWidth={1.8} aria-hidden />
            ) : (
              <MicOff size={30} strokeWidth={1.8} aria-hidden />
            )}
          </button>

          <p className="mt-4 text-[15px] font-medium text-ink">
            {busy
              ? "Working…"
              : isRecording
                ? "Recording"
                : openMeeting
                  ? "A meeting is open"
                  : "Ready to record"}
          </p>

          {isRecording ? (
            <p className="mt-1 font-mono text-[26px] tabular-nums text-ink" aria-live="off">
              {duration(elapsed)}
            </p>
          ) : openMeeting ? (
            <p className="mt-1 max-w-xs text-center text-[12.5px] leading-relaxed text-ink-muted">
              No audio is being captured. Press to close it, or import a recording into it.
            </p>
          ) : (
            <p className="mt-1 max-w-xs text-center text-[12.5px] leading-relaxed text-ink-muted">
              Audio is transcribed on this machine. Nothing is uploaded.
            </p>
          )}

          {isRecording && liveDevice && (
            <p className="mt-1 text-[12px] text-ink-faint">from {liveDevice}</p>
          )}
        </div>

        {!isRecording && !openMeeting && (
          <div className="mt-8 space-y-3">
            <label className="block">
              <span className="mb-1 block text-[12px] font-medium text-ink-muted">Input</span>
              <select
                value={preferredDevice ?? ""}
                onChange={(event) => onDeviceChange(event.target.value || null)}
                disabled={devices.length === 0}
                className="field disabled:cursor-not-allowed disabled:opacity-60"
              >
                <option value="">System default</option>
                {devices.map((device) => (
                  <option key={device.name} value={device.name}>
                    {device.name}
                    {device.is_default ? " (default)" : ""}
                  </option>
                ))}
              </select>
              {deviceError && (
                <span className="mt-1 block text-[11.5px] leading-snug text-warn-text">
                  {deviceError}
                </span>
              )}
              {!deviceError && devices.length === 0 && (
                <span className="mt-1 block text-[11.5px] text-ink-faint">
                  No inputs found.
                </span>
              )}
            </label>

            <p className="text-[11.5px] text-ink-faint">
              Language: {language ?? "detected automatically"} · Model:{" "}
              {health?.ai_model ?? "—"}
            </p>

            <button type="button" onClick={onImport} className="btn-quiet w-full">
              <Upload size={14} aria-hidden />
              Import an audio file instead
            </button>
          </div>
        )}

        {isRecording && (
          <div className="mt-8">
            <div className="mb-2 flex items-center gap-2">
              <Circle size={8} className="fill-record text-record" aria-hidden />
              <h2 className="flex-1 text-[12px] font-semibold text-ink">Coming through</h2>
              {recordingId && (
                <button
                  type="button"
                  onClick={() =>
                    onNavigate({ name: "meeting", id: recordingId, tab: "transcript" })
                  }
                  className="text-[12px] text-ink-muted transition hover:text-ink"
                >
                  Open the meeting
                </button>
              )}
            </div>

            <div className="card max-h-56 overflow-y-auto px-4 py-3">
              {recent.length === 0 ? (
                <p className="text-[12.5px] leading-relaxed text-ink-faint">
                  Listening. Text appears a few seconds behind the audio — transcription runs
                  in windows, not word by word.
                </p>
              ) : (
                <ul className="space-y-2">
                  {recent.map((segment) => (
                    <li key={segment.id} className="text-[13px] leading-relaxed">
                      {segment.speaker && (
                        <span className="mr-1.5 text-[11.5px] font-medium text-ink-faint">
                          {segment.speaker}
                        </span>
                      )}
                      <span className="text-ink">{segment.text}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
