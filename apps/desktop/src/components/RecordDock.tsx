import { useEffect, useState } from "react";
import { Mic, MicOff, MoreHorizontal, Square } from "lucide-react";

interface Props {
  isRecording: boolean;
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
  onSummarize: () => void;
  canSummarize: boolean;
  onExport: (variant: "full" | "brief") => void;
  canExport: boolean;
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
  startedAt,
  busy,
  canRecord,
  device,
  onToggle,
  onSummarize,
  canSummarize,
  onExport,
  canExport,
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
    : canRecord
      ? "Start recording"
      : "Start a meeting (this engine cannot capture audio)";

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-6 flex justify-center">
      <div
        className="pointer-events-auto relative flex items-center gap-1 rounded-full
                   border border-hairline bg-white px-2 py-2 shadow-dock"
      >
        <button
          type="button"
          onClick={onToggle}
          disabled={busy}
          aria-label={label}
          aria-pressed={isRecording}
          title={label}
          className={`flex h-11 w-11 items-center justify-center rounded-full text-white
                      transition disabled:opacity-60
                      ${
                        isRecording
                          ? "bg-record recording-pulse"
                          : canRecord
                            ? "bg-record hover:bg-record-hover"
                            : "bg-neutral-400 hover:bg-neutral-500"
                      }`}
        >
          {isRecording ? (
            <Square size={16} fill="currentColor" aria-hidden />
          ) : canRecord ? (
            <Mic size={19} strokeWidth={2} aria-hidden />
          ) : (
            <MicOff size={19} strokeWidth={2} aria-hidden />
          )}
        </button>

        {isRecording && startedAt !== null && (
          <span className="flex flex-col justify-center px-2 leading-tight">
            <span
              className="font-mono text-[13px] tabular-nums text-neutral-700"
              aria-live="off"
            >
              {elapsed(startedAt)}
            </span>
            {device && (
              // Which input is live matters: the usual recording failure is capturing the
              // wrong device and finding out afterwards.
              <span className="max-w-[13ch] truncate text-[10px] text-neutral-400" title={device}>
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
          className="flex h-9 w-9 items-center justify-center rounded-full text-neutral-400
                     transition hover:bg-neutral-100 hover:text-neutral-700"
        >
          <MoreHorizontal size={18} aria-hidden />
        </button>

        {menuOpen && (
          <div
            role="menu"
            className="absolute bottom-full right-0 mb-2 w-52 overflow-hidden rounded-xl
                       border border-hairline bg-white py-1 shadow-dock"
          >
            <button
              type="button"
              role="menuitem"
              disabled={!canSummarize}
              onClick={() => {
                setMenuOpen(false);
                onSummarize();
              }}
              className="w-full px-3 py-2 text-left text-[13px] text-neutral-700
                         transition hover:bg-neutral-50 disabled:cursor-not-allowed disabled:text-neutral-300"
            >
              Summarize this meeting
            </button>
            <p className="px-3 pb-1 pt-1 text-[11px] leading-snug text-neutral-400">
              {canSummarize
                ? "Runs on the configured backend."
                : "Needs a finished meeting with a transcript."}
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
              className="w-full px-3 py-2 text-left text-[13px] text-neutral-700
                         transition hover:bg-neutral-50 disabled:cursor-not-allowed disabled:text-neutral-300"
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
              className="w-full px-3 py-2 text-left text-[13px] text-neutral-700
                         transition hover:bg-neutral-50 disabled:cursor-not-allowed disabled:text-neutral-300"
            >
              Export summary only
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
