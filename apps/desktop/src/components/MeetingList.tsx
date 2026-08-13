import type { Meeting } from "../lib/api";

interface Props {
  meetings: Meeting[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

function when(iso: string): string {
  const date = new Date(iso);
  const today = new Date();
  const sameDay = date.toDateString() === today.toDateString();

  return sameDay
    ? date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
    : date.toLocaleDateString([], { month: "short", day: "numeric" });
}

/** The collapsible meeting list. Hidden by default so a live transcript gets the full width. */
export function MeetingList({ meetings, selectedId, onSelect }: Props) {
  return (
    <aside className="chrome w-64 shrink-0 overflow-y-auto border-r border-hairline bg-rail">
      <h2 className="px-4 pb-2 pt-4 text-[11px] font-semibold uppercase tracking-wide text-neutral-400">
        Meetings
      </h2>

      {meetings.length === 0 ? (
        <p className="px-4 text-[13px] text-neutral-400">Nothing recorded yet.</p>
      ) : (
        <ul className="pb-4">
          {meetings.map((meeting) => {
            const live = meeting.ended_at === null;
            return (
              <li key={meeting.id}>
                <button
                  type="button"
                  onClick={() => onSelect(meeting.id)}
                  aria-current={meeting.id === selectedId ? "true" : undefined}
                  className={`w-full px-4 py-2 text-left transition ${
                    meeting.id === selectedId ? "bg-neutral-100" : "hover:bg-neutral-50"
                  }`}
                >
                  <div className="flex items-center gap-1.5">
                    {live && (
                      <span
                        className="h-1.5 w-1.5 shrink-0 rounded-full bg-record"
                        aria-hidden
                      />
                    )}
                    <span className="truncate text-[13px] font-medium text-neutral-800">
                      {meeting.title}
                    </span>
                  </div>
                  <span className="text-[11px] text-neutral-400">
                    {live ? "Recording" : when(meeting.started_at)}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </aside>
  );
}
