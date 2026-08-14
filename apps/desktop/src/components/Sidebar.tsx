import { Info, PenLine, Settings, Waves } from "lucide-react";

export type View = "meetings" | "settings" | "about";

interface Props {
  view: View;
  onChange: (view: View) => void;
  /** Drives the rail's recording indicator without a second source of truth. */
  isRecording: boolean;
  /** Jump back to the meeting being recorded from anywhere in the app. */
  onGoLive: () => void;
}

const ITEMS: Array<{ id: View; label: string; Icon: typeof Info }> = [
  { id: "meetings", label: "Meetings", Icon: Waves },
  { id: "settings", label: "Settings", Icon: Settings },
  { id: "about", label: "About", Icon: Info },
];

/**
 * The narrow icon rail.
 *
 * Three items, not seven. Transcript, summary and chat were rail destinations until they became
 * tabs on the meeting itself — they are views of one thing, and promoting them to top-level
 * navigation made the rail look like a settings menu and made switching between them lose the
 * user's place. What is left is the app's actual top level: the meetings, its configuration,
 * and what it is.
 */
export function Sidebar({ view, onChange, isRecording, onGoLive }: Props) {
  return (
    <nav
      aria-label="Main"
      className="chrome flex w-[52px] shrink-0 flex-col items-center gap-1 border-r border-hairline bg-rail py-3"
    >
      <div className="mb-2 flex h-8 w-8 items-center justify-center text-record">
        <PenLine size={18} strokeWidth={2.2} aria-hidden />
        <span className="sr-only">Notewise</span>
      </div>

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
          <span className="h-2 w-2 rounded-full bg-white" aria-hidden />
        </button>
      )}
    </nav>
  );
}
