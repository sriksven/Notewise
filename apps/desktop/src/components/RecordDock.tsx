import { useEffect, useState } from "react";
import { Mic, MicOff, MoreHorizontal, Square } from "lucide-react";

interface Props {
  isRecording: boolean;
  /**
   * A meeting exists and has not been ended, but nothing is being captured into it.
   *
   * Only reachable on an engine that cannot record, where pressing record creates a meeting
   * to import a transcript into. The button has to say "End" there — labelling it "Record"
   * while the press would close the meeting is the worst of both.
   */
  openMeeting: boolean;
  startedAt: number | null;
  busy: boolean;
  /**
   * Whether the engine can actually capture audio.
   *
   * The button stays enabled when false — it still creates a meeting for an imported
   * transcript — but says what it will and will not do, rather than looking identical to a
   * build that records.
   */
  canRecord: boolean;
  /** The input the engine is capturing from, once it is recording. */
  device: string | null;
  onToggle: () => void;
  onExport: (variant: "full" | "brief") => void;
  canExport: boolean;
  onImport: () => void;
  /** False when the engine cannot transcribe, or is already busy recording. */
  canImport: boolean;
}

/** Elapsed time as mm:ss, or h:mm:ss once a meeting runs past an hour. */
function elapsed(sinceMs: number): string {
  const total = Math.max(0, Math.floor((Date.now() - sinceMs) / 1000));
  const s = String(total % 60).padStart(2, "0");
  const m = String(Math.floor(total / 60) % 60).padStart(2, "0");
  const h = Math.floor(total / 3600);
  return h > 0 ? `${h}:${m}:${s}` : `${m}:${s}`;
}

/**
 * The floating record control.
 *
 * Docked bottom-centre and always present, because starting and stopping is the
 * only thing a user must be able to do without hunting — including mid-sentence
 * while looking at someone else.
 */
export function RecordDock({
  isRecording,
  openMeeting,
  startedAt,
  busy,
  canRecord,
  device,
  onToggle,
  onExport,
  canExport,
  onImport,
  canImport,
}: Props) {
  const [, tick] = useState(0);
  const [menuOpen, setMenuOpen] = useState(false);

  // Re-render once a second only while recording; an idle dock does no work.
  useEffect(() => {
    if (!isRecording) return;
    const id = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [isRecording]);

  useEffect(() => {
    if (!menuOpen) return;
    const close = () => setMenuOpen(false);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [menuOpen]);

  const label = isRecording
    ? "Stop recording"
    : openMeeting
      ? "End this meeting"
      : canRecord
        ? "Start recording"
        : "Start a meeting (this engine cannot capture audio)";

  const word = busy ? "Working" : isRecording ? "Stop" : openMeeting ? "End" : "Record";

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-6 flex justify-center">
      <div
        className="pointer-events-auto relative flex items-center gap-1 rounded-full
                   border border-hairline bg-surface px-2 py-2 shadow-dock"
      >
        {/* Labelled, not a bare dot.
            Starting a recording is the one irreversible thing in the app — it opens a
            microphone — and a circle with an icon in it asks the user to remember which state
            they are in. The word says it, the colour reinforces it, and the shape changes so
            it reads at a glance and without colour. */}
        <button
          type="button"
          onClick={onToggle}
          disabled={busy}
          aria-label={label}
          aria-pressed={isRecording}
          title={label}
          className={`flex h-11 items-center gap-2.5 rounded-full pl-3.5 pr-4 text-[13px]
                      font-medium transition disabled:opacity-60
                      ${
                        isRecording
                          ? "bg-record text-white recording-pulse hover:bg-record-hover"
                          : openMeeting || !canRecord
                            ? "bg-ink-faint text-white hover:bg-ink-muted"
                            : "bg-record text-white hover:bg-record-hover"
                      }`}
        >
          <span className="flex h-5 w-5 items-center justify-center">
            {isRecording || openMeeting ? (
              // A square reads as stop without needing the colour, which matters for anyone
              // who cannot separate the red from the surface behind it.
              <Square size={13} fill="currentColor" aria-hidden />
            ) : canRecord ? (
              <Mic size={17} strokeWidth={2} aria-hidden />
            ) : (
              <MicOff size={17} strokeWidth={2} aria-hidden />
            )}
          </span>
          {word}
        </button>

        {isRecording && startedAt !== null && (
          <span className="flex flex-col justify-center px-2 leading-tight">
            <span
              className="font-mono text-[13px] tabular-nums text-ink"
              aria-live="off"
            >
              {elapsed(startedAt)}
            </span>
            {device && (
              // Which input is live matters: the usual recording failure is capturing the
              // wrong device and finding out afterwards.
              <span className="max-w-[13ch] truncate text-[10px] text-ink-faint" title={device}>
                {device}
              </span>
            )}
          </span>
        )}

        <button
          type="button"
          onClick={(event) => {
            // Without this the window listener closes the menu in the same tick.
            event.stopPropagation();
            setMenuOpen((open) => !open);
          }}
          aria-label="More options"
          aria-expanded={menuOpen}
          aria-haspopup="menu"
          className="flex h-9 w-9 items-center justify-center rounded-full text-ink-faint
                     transition hover:bg-overlay hover:text-ink"
        >
          <MoreHorizontal size={18} aria-hidden />
        </button>

        {menuOpen && (
          <div
            role="menu"
            className="absolute bottom-full right-0 mb-2 w-52 overflow-hidden rounded-xl
                       border border-hairline bg-surface py-1 shadow-dock"
          >
            <button
              type="button"
              role="menuitem"
              disabled={!canImport}
              onClick={() => {
                setMenuOpen(false);
                onImport();
              }}
              className="w-full px-3 py-2 text-left text-[13px] text-ink
                         transition hover:bg-overlay disabled:cursor-not-allowed disabled:text-ink-faint"
            >
              Import an audio file
            </button>
            <p className="px-3 pb-1 pt-1 text-[11px] leading-snug text-ink-faint">
              {canImport
                ? "Transcribes a WAV already on this machine."
                : "Needs a build that can transcribe."}
            </p>

            <div className="my-1 border-t border-hairline" />

            <button
              type="button"
              role="menuitem"
              disabled={!canExport}
              onClick={() => {
                setMenuOpen(false);
                onExport("full");
              }}
              className="w-full px-3 py-2 text-left text-[13px] text-ink
                         transition hover:bg-overlay disabled:cursor-not-allowed disabled:text-ink-faint"
            >
              Export as Markdown
            </button>

            <button
              type="button"
              role="menuitem"
              disabled={!canExport}
              onClick={() => {
                setMenuOpen(false);
                onExport("brief");
              }}
              className="w-full px-3 py-2 text-left text-[13px] text-ink
                         transition hover:bg-overlay disabled:cursor-not-allowed disabled:text-ink-faint"
            >
              Export summary only
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
