import {
  CalendarDays,
  Home,
  Info,
  Mic,
  Settings,
  PenLine,
} from "lucide-react";

export type View = "home" | "record" | "calendar" | "settings" | "about";

interface Props {
  view: View;
  onChange: (view: View) => void;
  /** Drives the rail's recording indicator without a second source of truth. */
  isRecording: boolean;
}

const ITEMS: Array<{ id: View; label: string; Icon: typeof Home }> = [
  { id: "home", label: "Home", Icon: Home },
  { id: "record", label: "Record", Icon: Mic },
  { id: "calendar", label: "Calendar", Icon: CalendarDays },
  { id: "settings", label: "Settings", Icon: Settings },
  { id: "about", label: "About", Icon: Info },
];

/**
 * The narrow icon rail.
 *
 * Icon-only at 52px: this is a five-item, permanent navigation, and labels would
 * cost horizontal space the transcript needs more. Each button carries an
 * accessible name so screen readers and tooltips are not left guessing.
 */
export function Sidebar({ view, onChange, isRecording }: Props) {
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

        // The record item is the one place colour is used, so the rail shows
        // capture state even when the user is on another view.
        const recordStyling =
          id === "record" && (isRecording || active)
            ? `bg-record text-white hover:bg-record-hover hover:text-white ${
                isRecording ? "recording-pulse" : ""
              }`
            : "";

        return (
          <button
            key={id}
            type="button"
            onClick={() => onChange(id)}
            aria-label={label}
            aria-current={active ? "page" : undefined}
            title={label}
            className={`rail-icon ${active ? "rail-icon-active" : ""} ${recordStyling}`}
          >
            <Icon size={18} strokeWidth={1.9} aria-hidden />
          </button>
        );
      })}
    </nav>
  );
}
