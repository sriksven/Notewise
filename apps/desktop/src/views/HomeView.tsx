import { useEffect, useState } from "react";
import {
  ArrowRight,
  Bot,
  FileText,
  Mic,
  SquareCheckBig,
  Upload,
  Waves,
} from "lucide-react";

import { api, type ActionItem, type Meeting, type Note } from "../lib/api";
import { relativeTime } from "../lib/format";
import type { Route } from "../lib/router";

interface Props {
  meetings: Meeting[];
  isRecording: boolean;
  canRecord: boolean;
  onNavigate: (route: Route) => void;
  onStartRecording: () => void;
  onImport: () => void;
}

/**
 * Time-of-day greeting.
 *
 * Local hours, from the machine's own clock. The cutoffs are the ordinary English ones rather
 * than anything clever — an app that says "good evening" at 4pm is more noticeable than one
 * that says nothing at all.
 */
function greeting(now = new Date()): string {
  const hour = now.getHours();
  if (hour < 5) return "Still up";
  if (hour < 12) return "Good morning";
  if (hour < 18) return "Good afternoon";
  return "Good evening";
}

/**
 * The landing page.
 *
 * Two jobs: start the next meeting, and get back into the last one. Everything else on this
 * screen is a shortcut to material the user already made — because a home page whose content
 * is all buttons is a menu, and a menu is what the sidebar is for.
 *
 * The empty state is not a decorative illustration. A workspace with nothing in it is the one
 * moment the app has to explain what it does, so it says what will happen when the button is
 * pressed.
 */
export function HomeView({
  meetings,
  isRecording,
  canRecord,
  onNavigate,
  onStartRecording,
  onImport,
}: Props) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [open, setOpen] = useState<ActionItem[]>([]);

  // Best-effort. This page is a launchpad; a failed side panel should not stop it rendering
  // the things that always work.
  useEffect(() => {
    let cancelled = false;

    void api
      .notes(5)
      .then((loaded) => !cancelled && setNotes(loaded))
      .catch(() => {});

    // Open work across the recent meetings. Asked per meeting because action items belong to
    // meetings; capped so a large workspace does not fan out into fifty requests.
    void Promise.all(
      meetings.slice(0, 8).map((meeting) => api.actionItems(meeting.id).catch(() => [])),
    )
      .then((lists) => {
        if (cancelled) return;
        setOpen(
          lists
            .flat()
            .filter((item) => item.status !== "done" && item.status !== "cancelled")
            .slice(0, 5),
        );
      })
      .catch(() => {});

    return () => {
      cancelled = true;
    };
  }, [meetings]);

  const recent = meetings.slice(0, 6);
  const empty = meetings.length === 0;

  return (
    <div className="flex-1 overflow-y-auto px-8 py-8">
      <div className="mx-auto max-w-3xl">
        <header className="mb-7">
          <h1 className="text-[24px] font-semibold tracking-tight text-ink">{greeting()}</h1>
          <p className="mt-1 text-[13px] text-ink-muted">
            {empty
              ? "Nothing here yet. Record a meeting and it will land below."
              : `${meetings.length} meeting${meetings.length === 1 ? "" : "s"} in this workspace.`}
          </p>
        </header>

        <div className="mb-8 grid grid-cols-2 gap-3">
          <button
            type="button"
            onClick={isRecording ? () => onNavigate({ name: "record" }) : onStartRecording}
            disabled={!canRecord && !isRecording}
            className="card flex items-center gap-3 px-4 py-4 text-left transition
                       hover:bg-overlay disabled:cursor-not-allowed disabled:opacity-50"
          >
            <span
              className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg ${
                isRecording ? "bg-record text-white" : "bg-accent text-accent-on"
              }`}
            >
              <Mic size={17} aria-hidden />
            </span>
            <span className="min-w-0">
              <span className="block text-[13.5px] font-medium text-ink">
                {isRecording ? "Recording now" : "Start recording"}
              </span>
              <span className="block truncate text-[12px] text-ink-muted">
                {isRecording
                  ? "Go to the live meeting"
                  : canRecord
                    ? "Capture from your microphone"
                    : "Not available in this build"}
              </span>
            </span>
          </button>

          <button
            type="button"
            onClick={onImport}
            disabled={isRecording}
            className="card flex items-center gap-3 px-4 py-4 text-left transition
                       hover:bg-overlay disabled:cursor-not-allowed disabled:opacity-50"
          >
            <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-overlay text-ink">
              <Upload size={17} aria-hidden />
            </span>
            <span className="min-w-0">
              <span className="block text-[13.5px] font-medium text-ink">Import audio</span>
              <span className="block truncate text-[12px] text-ink-muted">
                Transcribe a recording you already have
              </span>
            </span>
          </button>
        </div>

        <Section
          title="Recent meetings"
          icon={Waves}
          action={
            meetings.length > recent.length
              ? { label: "See all", onClick: () => onNavigate({ name: "library" }) }
              : undefined
          }
        >
          {recent.length === 0 ? (
            <Empty>
              Meetings appear here once you record or import one. Everything stays on this
              machine.
            </Empty>
          ) : (
            <ul className="card divide-y divide-hairline overflow-hidden">
              {recent.map((meeting) => (
                <li key={meeting.id}>
                  <button
                    type="button"
                    onClick={() =>
                      onNavigate({ name: "meeting", id: meeting.id, tab: "transcript" })
                    }
                    className="flex w-full items-center gap-3 px-4 py-2.5 text-left transition hover:bg-overlay"
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] text-ink">
                        {meeting.title}
                      </span>
                      <span className="block text-[11.5px] text-ink-faint">
                        {relativeTime(meeting.started_at)}
                        {meeting.ended_at === null && " · still open"}
                      </span>
                    </span>
                    <ArrowRight size={14} className="shrink-0 text-ink-faint" aria-hidden />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </Section>

        {open.length > 0 && (
          <Section
            title="Open work"
            icon={SquareCheckBig}
            action={{ label: "All tasks", onClick: () => onNavigate({ name: "tasks" }) }}
          >
            <ul className="card divide-y divide-hairline overflow-hidden">
              {open.map((item) => (
                <li key={item.id} className="px-4 py-2.5">
                  <p className="text-[13px] text-ink">{item.text}</p>
                  {item.owner && (
                    <p className="mt-0.5 text-[11.5px] text-ink-faint">{item.owner}</p>
                  )}
                </li>
              ))}
            </ul>
          </Section>
        )}

        {notes.length > 0 && (
          <Section
            title="Recent notes"
            icon={FileText}
            action={{ label: "All notes", onClick: () => onNavigate({ name: "notes" }) }}
          >
            <ul className="card divide-y divide-hairline overflow-hidden">
              {notes.map((note) => (
                <li key={note.id}>
                  <button
                    type="button"
                    onClick={() => onNavigate({ name: "notes", id: note.id })}
                    className="flex w-full items-center gap-3 px-4 py-2.5 text-left transition hover:bg-overlay"
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] text-ink">
                        {note.title || "Untitled"}
                      </span>
                      <span className="block text-[11.5px] text-ink-faint">
                        {relativeTime(note.updated_at)}
                      </span>
                    </span>
                    <ArrowRight size={14} className="shrink-0 text-ink-faint" aria-hidden />
                  </button>
                </li>
              ))}
            </ul>
          </Section>
        )}

        {!empty && (
          <button
            type="button"
            onClick={() => onNavigate({ name: "agent" })}
            className="card mt-2 flex w-full items-center gap-3 px-4 py-3 text-left transition hover:bg-overlay"
          >
            <Bot size={16} className="shrink-0 text-ink-faint" aria-hidden />
            <span className="min-w-0 flex-1">
              <span className="block text-[13px] font-medium text-ink">
                Ask the agent to look something up
              </span>
              <span className="block text-[12px] text-ink-muted">
                It reads across your meetings and writes up what it finds.
              </span>
            </span>
            <ArrowRight size={14} className="shrink-0 text-ink-faint" aria-hidden />
          </button>
        )}
      </div>
    </div>
  );
}

function Section({
  title,
  icon: Icon,
  action,
  children,
}: {
  title: string;
  icon: typeof Waves;
  action?: { label: string; onClick: () => void };
  children: React.ReactNode;
}) {
  return (
    <section className="mb-7">
      <div className="mb-2 flex items-center gap-2">
        <Icon size={14} className="text-ink-faint" aria-hidden />
        <h2 className="flex-1 text-[12.5px] font-semibold text-ink">{title}</h2>
        {action && (
          <button
            type="button"
            onClick={action.onClick}
            className="text-[12px] text-ink-muted transition hover:text-ink"
          >
            {action.label}
          </button>
        )}
      </div>
      {children}
    </section>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return (
    <p className="card px-4 py-5 text-center text-[12.5px] leading-relaxed text-ink-muted">
      {children}
    </p>
  );
}
