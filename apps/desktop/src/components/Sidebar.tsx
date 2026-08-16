import { FileText, Info, Settings, TicketCheck, Waves } from "lucide-react";

import { Logo } from "./Logo";

export type View = "meetings" | "notes" | "tickets" | "settings" | "about";

interface Props {
  view: View;
  onChange: (view: View) => void;
  /** Drives the rail's recording indicator without a second source of truth. */
  isRecording: boolean;
  /** Jump back to the meeting being recorded from anywhere in the app. */
  onGoLive: () => void;
  /** The mark goes home, which is what a logo in the corner is for. */
  onHome: () => void;
}

const ITEMS: Array<{ id: View; label: string; Icon: typeof Info }> = [
  { id: "meetings", label: "Meetings", Icon: Waves },
  { id: "notes", label: "Notes", Icon: FileText },
  { id: "tickets", label: "Tickets", Icon: TicketCheck },
  { id: "settings", label: "Settings", Icon: Settings },
  { id: "about", label: "About", Icon: Info },
];

/**
 * The narrow icon rail.
 *
 * Five items, not seven. Transcript, summary and chat were rail destinations until they became
 * tabs on the meeting itself — they are views of one thing, and promoting them to top-level
 * navigation made the rail look like a settings menu and made switching between them lose the
 * user's place. What is left is the app's actual top level: the meetings, the work they
 * produced, its configuration, and what it is.
 *
 * Notes and tickets earn rail slots rather than tabs because they outlive the meeting that
 * created them. Work filed on Monday is chased on Thursday, and reaching it should not
 * require remembering which meeting it came out of.
 */
export function Sidebar({ view, onChange, isRecording, onGoLive, onHome }: Props) {
  return (
    <nav
      aria-label="Main"
      className="chrome flex w-[52px] shrink-0 flex-col items-center gap-1 border-r border-hairline bg-rail py-3"
    >
      <button
        type="button"
        onClick={onHome}
        aria-label="Notewise — go to meetings"
        title="Notewise"
        className="mb-2 flex h-8 w-8 items-center justify-center rounded-lg text-accent
                   transition hover:bg-overlay"
      >
        <Logo size={19} />
      </button>

      {ITEMS.map(({ id, label, Icon }) => {
        const active = view === id;
        return (
          <button
            key={id}
            type="button"
            onClick={() => onChange(id)}
            aria-label={label}
            aria-current={active ? "page" : undefined}
            title={label}
            className={`rail-icon ${active ? "rail-icon-active" : ""}`}
          >
            <Icon size={18} strokeWidth={1.9} aria-hidden />
          </button>
        );
      })}

      {/* Capture state, wherever the user has navigated to. The rail is the only thing on
          screen from every view, so it is the only place this can always be seen — and it
          doubles as the way back to the meeting that is running. */}
      {isRecording && (
        <button
          type="button"
          onClick={onGoLive}
          aria-label="Go to the meeting being recorded"
          title="Recording — go to the live meeting"
          className="mt-auto flex h-8 w-8 items-center justify-center rounded-full
                     bg-record text-white transition recording-pulse hover:bg-record-hover"
        >
          <span className="h-2 w-2 rounded-full bg-surface" aria-hidden />
        </button>
      )}
    </nav>
  );
}
