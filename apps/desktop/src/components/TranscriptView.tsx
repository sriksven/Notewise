import { useEffect, useRef } from "react";
import type { Segment } from "../lib/api";

interface Props {
  segments: Segment[];
  isRecording: boolean;
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
export function TranscriptView({ segments, isRecording }: Props) {
  const endRef = useRef<HTMLDivElement>(null);

  // Follow the tail while recording. Not while idle: a user scrolling back
  // through an old transcript should not be yanked to the bottom.
  useEffect(() => {
    if (isRecording) endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [segments.length, isRecording]);

  if (segments.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center px-6 text-center">
        <h1 className="text-[22px] font-semibold tracking-tight text-neutral-900">
          Welcome to Notewise
        </h1>
        <p className="mt-1.5 text-[13px] text-neutral-500">
          {isRecording
            ? "Listening — transcript will appear here."
            : "Start recording to see live transcription"}
        </p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto px-8 py-6">
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
