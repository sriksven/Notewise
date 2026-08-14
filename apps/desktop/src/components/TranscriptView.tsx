import { useEffect, useRef } from "react";
import { AudioLines } from "lucide-react";

import type { Segment } from "../lib/api";

interface Props {
  segments: Segment[];
  isRecording: boolean;
  /** False when nothing is selected, which is a different emptiness from an empty meeting. */
  hasMeeting: boolean;
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
export function TranscriptView({ segments, isRecording, hasMeeting }: Props) {
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
          className={isRecording ? "text-record" : "text-neutral-300"}
          aria-hidden
        />
        <p className="mt-3 max-w-xs text-[13px] leading-relaxed text-neutral-500">{message}</p>
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

          return (
            <div key={segment.id} className={sameSpeaker ? "-mt-3.5" : ""}>
              {!sameSpeaker && (
                <div className="mb-1 flex items-baseline gap-2">
                  <span className="text-[13px] font-semibold text-neutral-900">
                    {segment.speaker ?? "Unattributed"}
                  </span>
                  <span className="font-mono text-[11px] tabular-nums text-neutral-400">
                    {timestamp(segment.start_ms)}
                  </span>
                </div>
              )}
              <p className="text-[14px] leading-relaxed text-neutral-700">
                {segment.text}
              </p>
            </div>
          );
        })}
        <div ref={endRef} />
      </div>
    </div>
  );
}
