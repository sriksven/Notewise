import { useEffect, useRef, useState } from "react";
import { AudioLines } from "lucide-react";

import type { Segment, Speaker } from "../lib/api";
import { SpeakerName } from "./SpeakerName";

interface Props {
  segments: Segment[];
  isRecording: boolean;
  /** False when nothing is selected, which is a different emptiness from an empty meeting. */
  hasMeeting: boolean;
  /** The distinct voices, for naming them. Empty until loaded, which only costs the affordance. */
  speakers?: Speaker[];
  onRenameSpeaker?: (from: string | null, to: string) => Promise<void>;
  /**
   * Correct a mis-transcribed line.
   *
   * Optional, so a caller with no way to persist a correction simply renders read-only text rather
   * than offering an edit that silently does nothing.
   */
  onCorrectSegment?: (segmentId: string, text: string) => Promise<void>;
}

function timestamp(ms: number): string {
  const total = Math.floor(ms / 1000);
  const s = String(total % 60).padStart(2, "0");
  const m = String(Math.floor(total / 60)).padStart(2, "0");
  return `${m}:${s}`;
}

/**
 * The live transcript.
 *
 * Consecutive segments from one speaker are grouped so the transcript reads as
 * paragraphs rather than a timestamped log — a wall of per-segment speaker
 * labels is technically accurate and much harder to read back later.
 */
export function TranscriptView({
  segments,
  isRecording,
  hasMeeting,
  speakers = [],
  onRenameSpeaker,
  onCorrectSegment,
}: Props) {
  const endRef = useRef<HTMLDivElement>(null);

  // Follow the tail while recording. Not while idle: a user scrolling back
  // through an old transcript should not be yanked to the bottom.
  useEffect(() => {
    if (isRecording) endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [segments.length, isRecording]);

  if (segments.length === 0) {
    // Three different nothings, and they call for three different things from the user. A
    // single "no transcript" message would leave someone who has not selected a meeting
    // looking for a bug.
    const message = isRecording
      ? "Listening. Speech appears here a few seconds behind the room."
      : hasMeeting
        ? "This meeting has no transcript. Import an audio file from the ⋯ menu, or record a new one."
        : "Press the red button to start a meeting, or pick one from the left.";

    return (
      <div className="flex flex-1 flex-col items-center justify-center px-6 pb-24 text-center">
        <AudioLines
          size={26}
          strokeWidth={1.4}
          className={isRecording ? "text-record" : "text-ink-faint"}
          aria-hidden
        />
        <p className="mt-3 max-w-xs text-[13px] leading-relaxed text-ink-muted">{message}</p>
      </div>
    );
  }

  return (
    // The tail clears the floating record dock, so the last thing said is never hidden behind
    // the button that stops the recording producing it.
    <div className="flex-1 overflow-y-auto px-8 pb-28 pt-6">
      <div className="mx-auto max-w-2xl space-y-5">
        {segments.map((segment, index) => {
          const previous = segments[index - 1];
          const sameSpeaker =
            previous && previous.speaker === segment.speaker && segment.speaker !== null;
          const speaker = speakers.find((s) => s.label === segment.speaker);

          return (
            <div key={segment.id} className={sameSpeaker ? "-mt-3.5" : ""}>
              {!sameSpeaker && (
                <div className="mb-1 flex items-baseline gap-2">
                  {speaker && onRenameSpeaker ? (
                    <SpeakerName
                      speaker={speaker}
                      all={speakers}
                      onRename={onRenameSpeaker}
                      // Not while recording: labels are still being assigned, and a name typed
                      // onto a cluster that is about to be relabelled would be quietly undone.
                      editable={!isRecording}
                    />
                  ) : (
                    <span className="text-[13px] font-semibold text-ink">
                      {segment.speaker ?? "Unattributed"}
                    </span>
                  )}
                  <span className="font-mono text-[11px] tabular-nums text-ink-faint">
                    {timestamp(segment.start_ms)}
                  </span>
                </div>
              )}
              {onCorrectSegment ? (
                <CorrectableLine
                  key={`${segment.id}:${segment.text}`}
                  text={segment.text}
                  onSave={(next) => onCorrectSegment(segment.id, next)}
                />
              ) : (
                <p className="text-[14px] leading-relaxed text-ink">{segment.text}</p>
              )}
            </div>
          );
        })}
        <div ref={endRef} />
      </div>
    </div>
  );
}

/**
 * One transcript line, correctable in place.
 *
 * Double-click to edit, Enter to save, Escape to abandon. Deliberately not a visible pencil on every
 * line: a transcript is mostly read, and hundreds of edit affordances would drown the thing being
 * read. The `title` says how, for anyone who does not guess.
 *
 * Empty is refused locally as well as by the engine — blanking a line leaves a gap with no record
 * anything was there, which is a different operation from correcting one.
 */
function CorrectableLine({
  text,
  onSave,
}: {
  text: string;
  onSave: (next: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(text);
  const [busy, setBusy] = useState(false);

  async function commit() {
    const next = draft.trim();
    if (next === "" || next === text) {
      setDraft(text);
      setEditing(false);
      return;
    }
    setBusy(true);
    try {
      await onSave(next);
      setEditing(false);
    } catch {
      // Leave the field open with the attempted text, so a failure does not discard typing.
    } finally {
      setBusy(false);
    }
  }

  if (!editing) {
    return (
      <p
        className="cursor-text text-[14px] leading-relaxed text-ink"
        title="Double-click to correct"
        onDoubleClick={() => setEditing(true)}
      >
        {text}
      </p>
    );
  }

  return (
    <textarea
      autoFocus
      disabled={busy}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => void commit()}
      onKeyDown={(e) => {
        if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          void commit();
        }
        if (e.key === "Escape") {
          setDraft(text);
          setEditing(false);
        }
      }}
      rows={Math.max(1, Math.ceil(draft.length / 80))}
      className="w-full resize-none rounded-md border border-accent/40 bg-transparent px-2 py-1 text-[14px] leading-relaxed text-ink outline-none"
    />
  );
}
