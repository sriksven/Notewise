import {
  CalendarClock,
  Bot,
  CircleHelp,
  FileText,
  Home,
  Info,
  Library,
  Mic,
  Plug,
  Settings,
  SquareCheckBig,
  TicketCheck,
  Trash2,
} from "lucide-react";

import { Logo } from "./Logo";
import type { Route } from "../lib/router";

/** Which top-level destination is showing. A meeting counts as the library. */
export type View = Route["name"];

interface Props {
  view: View;
  onNavigate: (route: Route) => void;
  /** Drives the recording indicator without a second source of truth. */
  isRecording: boolean;
  /** Jump back to the meeting being recorded from anywhere in the app. */
  onGoLive: () => void;
}

interface Item {
  route: Route;
  label: string;
  Icon: typeof Info;
  /** Other route names that should light this item up. */
  also?: View[];
}

/**
 * Three groups, because they answer three different questions.
 *
 * *Capture* is what the app does while a meeting is happening — the two things a user reaches
 * for with a call already ringing. *Workspace* is the material that outlives the meeting.
 * *Below the fold* is everything you go to deliberately and rarely.
 *
 * Search is absent on purpose: it lives in the top bar, where it can be reached with a
 * keystroke from any screen, rather than being a place you navigate to.
 */
const GROUPS: Array<{ label: string | null; items: Item[] }> = [
  {
    label: null,
    items: [
      { route: { name: "home" }, label: "Home", Icon: Home },
      { route: { name: "record" }, label: "Record", Icon: Mic },
      {
        route: { name: "library" },
        label: "Library",
        Icon: Library,
        // A meeting page is a page *in* the library; the sidebar should not go dark when
        // one is open.
        also: ["meeting"],
      },
    ],
  },
  {
    label: "Workspace",
    items: [
      { route: { name: "notes" }, label: "Notes", Icon: FileText },
      { route: { name: "tasks" }, label: "My Tasks", Icon: SquareCheckBig },
      { route: { name: "tickets" }, label: "Tickets", Icon: TicketCheck },
      { route: { name: "agent" }, label: "Agent", Icon: Bot },
      { route: { name: "jobs" }, label: "Automation", Icon: CalendarClock },
      { route: { name: "connectors" }, label: "Connectors", Icon: Plug },
    ],
  },
  {
    label: null,
    items: [
      { route: { name: "trash" }, label: "Trash", Icon: Trash2 },
      { route: { name: "help" }, label: "Help", Icon: CircleHelp },
      { route: { name: "settings" }, label: "Settings", Icon: Settings },
      { route: { name: "about" }, label: "About", Icon: Info },
    ],
  },
];

/**
 * The sidebar.
 *
 * Labelled rather than an icon rail. The rail worked at five destinations and stopped working
 * at twelve: an icon is a mnemonic for something you have already learned the position of, and
 * a column of twelve unlabelled glyphs is a memory test. The width buys grouping too, which is
 * what actually makes twelve items navigable.
 */
export function Sidebar({ view, onNavigate, isRecording, onGoLive }: Props) {
  const isActive = (item: Item) =>
    view === item.route.name || (item.also?.includes(view) ?? false);

  return (
    <nav
      aria-label="Main"
      className="chrome flex w-[196px] shrink-0 flex-col gap-0.5 overflow-y-auto border-r
                 border-hairline bg-rail px-2 py-3"
    >
      <button
        type="button"
        onClick={() => onNavigate({ name: "home" })}
        className="mb-2 flex items-center gap-2 rounded-lg px-2 py-1.5 text-left
                   transition hover:bg-overlay"
      >
        <span className="text-accent">
          <Logo size={18} />
        </span>
        <span className="text-[13px] font-semibold tracking-tight text-ink">Notewise</span>
      </button>

      {GROUPS.map((group, index) => (
        <div key={group.label ?? `group-${index}`} className={index > 0 ? "mt-3" : undefined}>
          {group.label && (
            <h2 className="px-2 pb-1 text-[10.5px] font-semibold uppercase tracking-wider text-ink-faint">
              {group.label}
            </h2>
          )}
          {group.items.map((item) => {
            const active = isActive(item);
            return (
              <button
                key={item.label}
                type="button"
                onClick={() => onNavigate(item.route)}
                aria-current={active ? "page" : undefined}
                className={`flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left
                            text-[13px] transition-colors ${
                              active
                                ? "bg-overlay font-medium text-ink"
                                : "text-ink-muted hover:bg-overlay hover:text-ink"
                            }`}
              >
                <item.Icon size={15} strokeWidth={1.9} className="shrink-0" aria-hidden />
                {item.label}
              </button>
            );
          })}
        </div>
      ))}

      {/* Capture state, wherever the user has navigated to. The sidebar is the only thing on
          screen from every view, so it is the only place this can always be seen — and it
          doubles as the way back to the meeting that is running. */}
      {isRecording && (
        <button
          type="button"
          onClick={onGoLive}
          className="mt-auto flex items-center gap-2 rounded-lg bg-record/10 px-2 py-2
                     text-left text-[12px] font-medium text-record transition
                     hover:bg-record/15"
        >
          <span
            className="recording-pulse h-2 w-2 shrink-0 rounded-full bg-record"
            aria-hidden
          />
          Recording — go live
        </button>
      )}
    </nav>
  );
}
