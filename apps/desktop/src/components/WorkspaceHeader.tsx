import { useState } from "react";
import { PanelRightOpen, Square, Trash2, X } from "lucide-react";

import type { Meeting, Segment } from "../lib/api";

import type { MeetingTab } from "../lib/router";

export type Tab = MeetingTab;

interface Props {
  meeting: Meeting | null;
  segments: Segment[];
  tab: Tab;
  onTabChange: (tab: Tab) => void;
  isRecording: boolean;
  /**
   * Stop capture from here.
   *
   * Passed only on the tabs where the floating dock is not on screen. Recording is the one
   * thing that must always be stoppable in one press — being three tabs deep in a chat is no
   * reason to have to go and find the button.
   */
  onStop?: () => void;
  /** Shown only when the panel is hidden, so re-opening it is never a dead end. */
  panelHidden: boolean;
  onShowPanel: () => void;
  /**
   * Move this meeting to the trash. Absent while it is recording.
   *
   * Recoverable, which is what makes the button defensible at all: a meeting owns its
   * transcript, summaries, decisions and action items, and all of them go with it.
   */
  onDelete?: () => void;
}

const TABS: Array<{ id: Tab; label: string }> = [
  { id: "transcript", label: "Transcript" },
  { id: "summary", label: "Summary" },
  // Between the machine's record of the meeting and questions about it sits the user's own
  // account of it, which is the one thing a transcript can never contain.
  { id: "notes", label: "Notes" },
  { id: "ask", label: "Ask" },
];

/** Wall-clock span of a meeting, or how long it has been running. */
function duration(meeting: Meeting): string | null {
  const start = new Date(meeting.started_at).getTime();
  const end = meeting.ended_at ? new Date(meeting.ended_at).getTime() : Date.now();
  const minutes = Math.round((end - start) / 60_000);

  if (!Number.isFinite(minutes) || minutes < 1) return null;
  if (minutes < 60) return `${minutes} min`;

  const hours = Math.floor(minutes / 60);
  return `${hours} h ${minutes % 60} min`;
}

/**
 * The meeting being worked on, and the three ways of looking at it.
 *
 * Tabs rather than destinations in the navigation rail. Transcript, summary and chat are three
 * views of one meeting, not three places in the app; routing them through the rail meant
 * switching view also lost track of which meeting was being read.
 */
export function WorkspaceHeader({
  meeting,
  segments,
  tab,
  onTabChange,
  isRecording,
  onStop,
  panelHidden,
  onShowPanel,
  onDelete,
}: Props) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const speakers = new Set(
    segments.map((segment) => segment.speaker).filter((name): name is string => name !== null),
  );

  const meta: string[] = [];
  if (meeting) {
    meta.push(
      new Date(meeting.started_at).toLocaleString([], {
        month: "short",
        day: "numeric",
        hour: "numeric",
        minute: "2-digit",
      }),
    );
    const span = duration(meeting);
    if (span) meta.push(span);
    if (speakers.size > 0) {
      meta.push(`${speakers.size} speaker${speakers.size === 1 ? "" : "s"}`);
    }
  }

  return (
    <header className="shrink-0 border-b border-hairline px-6 pt-4">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            {isRecording && (
              <span
                className="h-2 w-2 shrink-0 rounded-full bg-record recording-pulse"
                aria-hidden
              />
            )}
            <h1 className="truncate text-[17px] font-semibold tracking-tight text-ink">
              {meeting?.title ?? "No meeting selected"}
            </h1>
          </div>

          <p className="mt-0.5 h-[15px] text-[12px] text-ink-faint">
            {meeting ? meta.join(" · ") : "Pick one on the left, or press record."}
          </p>
        </div>

        {isRecording && onStop && (
          <button
            type="button"
            onClick={onStop}
            className="flex shrink-0 items-center gap-1.5 rounded-full bg-record px-3 py-1.5
                       text-[12px] font-medium text-white transition hover:bg-record-hover"
          >
            <Square size={11} fill="currentColor" aria-hidden />
            Stop
          </button>
        )}

        {/* Two presses, not a modal. A dialog over the meeting is heavier than this deserves
            now that the delete is reversible, and an inline confirm keeps what is about to go
            on screen while you decide. */}
        {onDelete &&
          !isRecording &&
          meeting &&
          (confirmingDelete ? (
            <span className="flex shrink-0 items-center gap-1.5">
              <span className="text-[12px] text-ink-muted">Move to trash?</span>
              <button
                type="button"
                onClick={() => {
                  setConfirmingDelete(false);
                  onDelete();
                }}
                className="rounded-full bg-danger-line px-2.5 py-1 text-[12px] font-medium
                           text-danger-text transition hover:opacity-90"
              >
                Delete
              </button>
              <button
                type="button"
                onClick={() => setConfirmingDelete(false)}
                aria-label="Cancel"
                className="text-ink-faint transition hover:text-ink"
              >
                <X size={13} aria-hidden />
              </button>
            </span>
          ) : (
            <button
              type="button"
              onClick={() => setConfirmingDelete(true)}
              aria-label="Move this meeting to the trash"
              title="Move to trash"
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg
                         border border-hairline bg-surface text-ink-muted transition
                         hover:bg-overlay hover:text-danger-text"
            >
              <Trash2 size={13} aria-hidden />
            </button>
          ))}

        {panelHidden && (
          <button
            type="button"
            onClick={onShowPanel}
            aria-label="Show the intelligence panel"
            title="Show decisions, action items and suggested questions"
            className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg
                       border border-hairline bg-surface text-ink-muted transition
                       hover:bg-overlay hover:text-ink"
          >
            <PanelRightOpen size={14} aria-hidden />
          </button>
        )}
      </div>

      <nav aria-label="Meeting views" className="-mb-px mt-3 flex gap-4">
        {TABS.map(({ id, label }) => {
          const active = tab === id;
          return (
            <button
              key={id}
              type="button"
              onClick={() => onTabChange(id)}
              aria-current={active ? "page" : undefined}
              className={`border-b-2 pb-2 text-[13px] transition ${
                active
                  ? "border-accent font-medium text-ink"
                  : "border-transparent text-ink-muted hover:text-ink"
              }`}
            >
              {label}
            </button>
          );
        })}
      </nav>
    </header>
  );
}
