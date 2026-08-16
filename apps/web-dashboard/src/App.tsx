import { useEffect, useState } from "react";
import {
  AlertTriangle,
  CalendarClock,
  CircleAlert,
  FileText,
  Gavel,
  Loader2,
  SquareCheckBig,
  Waves,
} from "lucide-react";

import { api, ApiError, type ActionItem, type Health, type Meeting, type Note, type Ticket } from "./lib/api";
import {
  isOpen,
  loadByOwner,
  meetingsPerDay,
  readableMinutes,
  totals,
  type DayCount,
  type OwnerLoad,
  type Totals,
} from "./lib/metrics";

/** How many meetings back to gather per-meeting detail from. */
const DETAIL_DEPTH = 60;

interface Workspace {
  health: Health;
  meetings: Meeting[];
  notes: Note[];
  actionItems: ActionItem[];
  tickets: Ticket[];
  decisions: number;
}

/**
 * A read-only overview of one workspace.
 *
 * Deliberately not a second copy of the desktop app. It answers questions the desktop app
 * cannot, because the desktop app is built around one meeting at a time: how much time went
 * into meetings, whether the rate is rising, who is carrying open work, and what is overdue.
 *
 * Nothing here writes. See `lib/api.ts` — the client contains no method that could.
 */
export function App() {
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    void (async () => {
      try {
        const [health, meetings, notes, tickets] = await Promise.all([
          api.health(),
          api.meetings(),
          api.notes(),
          api.tickets(),
        ]);

        // Action items and decisions are per-meeting, so this fans out. Bounded, and the
        // failures are swallowed per meeting: one unreadable meeting should not blank the
        // whole page.
        const recent = meetings.slice(0, DETAIL_DEPTH);
        const [itemLists, decisionLists] = await Promise.all([
          Promise.all(recent.map((m) => api.actionItems(m.id).catch(() => []))),
          Promise.all(recent.map((m) => api.decisions(m.id).catch(() => []))),
        ]);

        if (cancelled) return;
        setWorkspace({
          health,
          meetings,
          notes,
          tickets,
          actionItems: itemLists.flat(),
          decisions: decisionLists.flat().length,
        });
        setError(null);
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof ApiError ? e.message : "Could not load the workspace.");
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  if (error) {
    return (
      <Centred>
        <AlertTriangle size={22} className="text-warn-text" aria-hidden />
        <p className="text-[14px] font-medium text-ink">{error}</p>
        <p className="max-w-sm text-center text-[12.5px] leading-relaxed text-ink-muted">
          This dashboard reads a running Notewise engine on this machine. Start it with{" "}
          <code className="text-ink">notewise serve</code>.
        </p>
      </Centred>
    );
  }

  if (!workspace) {
    return (
      <Centred>
        <Loader2 size={20} className="animate-spin text-ink-faint" aria-hidden />
        <p className="text-[13px] text-ink-muted">Reading the workspace…</p>
      </Centred>
    );
  }

  const counts = totals(workspace);
  const perDay = meetingsPerDay(workspace.meetings, 30);
  const owners = loadByOwner([...workspace.actionItems, ...workspace.tickets]);

  return (
    <div className="min-h-full bg-bg px-6 py-8 sm:px-10">
      <div className="mx-auto max-w-5xl">
        <header className="mb-8">
          <h1 className="text-[22px] font-semibold tracking-tight text-ink">Workspace</h1>
          <p className="mt-1 text-[12.5px] text-ink-muted">
            Read-only · engine v{workspace.health.version} ·{" "}
            {workspace.health.ai_local ? "processing on this machine" : "using a hosted model"}
          </p>
        </header>

        <Summary counts={counts} />

        <section className="mb-8">
          <h2 className="mb-2 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
            <CalendarClock size={14} className="text-ink-faint" aria-hidden />
            Meetings, last 30 days
          </h2>
          <div className="card px-4 py-4">
            <Sparkline days={perDay} />
          </div>
        </section>

        <section className="mb-8">
          <h2 className="mb-2 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
            <SquareCheckBig size={14} className="text-ink-faint" aria-hidden />
            Who is carrying open work
          </h2>
          <OwnerTable owners={owners} />
        </section>

        <section>
          <h2 className="mb-2 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
            <Waves size={14} className="text-ink-faint" aria-hidden />
            Recent meetings
          </h2>
          <RecentMeetings meetings={workspace.meetings.slice(0, 8)} items={workspace.actionItems} />
        </section>
      </div>
    </div>
  );
}

function Centred({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-3 bg-bg px-6">
      {children}
    </div>
  );
}

function Summary({ counts }: { counts: Totals }) {
  const cards: Array<{ label: string; value: string; note?: string; Icon: typeof Waves; warn?: boolean }> = [
    { label: "Meetings", value: `${counts.meetings}`, note: readableMinutes(counts.minutes), Icon: Waves },
    { label: "Notes", value: `${counts.notes}`, Icon: FileText },
    { label: "Decisions", value: `${counts.decisions}`, Icon: Gavel },
    {
      label: "Open work",
      value: `${counts.openWork}`,
      note: counts.overdue > 0 ? `${counts.overdue} overdue` : undefined,
      Icon: counts.overdue > 0 ? CircleAlert : SquareCheckBig,
      warn: counts.overdue > 0,
    },
  ];

  return (
    <div className="mb-8 grid grid-cols-2 gap-3 sm:grid-cols-4">
      {cards.map((card) => (
        <div key={card.label} className="card px-4 py-3">
          <div className="flex items-center gap-1.5 text-[11.5px] text-ink-muted">
            <card.Icon size={12} aria-hidden />
            {card.label}
          </div>
          <p className="mt-1 text-[24px] font-semibold tabular-nums leading-none text-ink">
            {card.value}
          </p>
          {card.note && (
            <p className={`mt-1 text-[11.5px] ${card.warn ? "text-warn-text" : "text-ink-faint"}`}>
              {card.note}
            </p>
          )}
        </div>
      ))}
    </div>
  );
}

/**
 * A bar per day.
 *
 * Hand-drawn rather than pulling in a charting library: this is thirty rectangles and a
 * baseline, and a chart dependency would be larger than the entire rest of this app.
 */
function Sparkline({ days }: { days: DayCount[] }) {
  const peak = Math.max(1, ...days.map((day) => day.count));
  const busiest = days.reduce((sum, day) => sum + day.count, 0);

  return (
    <div>
      <div className="flex h-24 items-end gap-[3px]">
        {days.map((day) => (
          <div
            key={day.day}
            className="group relative flex-1"
            title={`${day.day}: ${day.count} meeting${day.count === 1 ? "" : "s"}`}
          >
            <div
              // A visible sliver for an empty day, so the axis reads as a row of days rather
              // than as a gap in the data.
              className={`w-full rounded-sm ${day.count > 0 ? "bg-accent" : "bg-hairline"}`}
              style={{ height: `${day.count > 0 ? (day.count / peak) * 88 + 8 : 3}px` }}
            />
          </div>
        ))}
      </div>
      <div className="mt-2 flex items-baseline justify-between text-[11px] text-ink-faint">
        <span>{days[0]?.day}</span>
        <span className="text-ink-muted">
          {busiest} in 30 days · busiest day {peak}
        </span>
        <span>{days.at(-1)?.day}</span>
      </div>
    </div>
  );
}

function OwnerTable({ owners }: { owners: OwnerLoad[] }) {
  if (owners.length === 0) {
    return (
      <p className="card px-4 py-5 text-center text-[12.5px] text-ink-muted">
        Nothing open. Every commitment so far has been closed.
      </p>
    );
  }

  const peak = Math.max(...owners.map((owner) => owner.open));

  return (
    <ul className="card divide-y divide-hairline overflow-hidden">
      {owners.map((owner) => (
        <li key={owner.owner} className="flex items-center gap-3 px-4 py-2.5">
          <span className="w-32 shrink-0 truncate text-[13px] text-ink">{owner.owner}</span>
          <span className="h-2 flex-1 overflow-hidden rounded-full bg-overlay">
            <span
              className="block h-full rounded-full bg-accent"
              style={{ width: `${(owner.open / peak) * 100}%` }}
            />
          </span>
          <span className="w-24 shrink-0 text-right text-[12px] tabular-nums text-ink-muted">
            {owner.open} open
            {owner.overdue > 0 && (
              <span className="text-warn-text"> · {owner.overdue} late</span>
            )}
          </span>
        </li>
      ))}
    </ul>
  );
}

function RecentMeetings({ meetings, items }: { meetings: Meeting[]; items: ActionItem[] }) {
  if (meetings.length === 0) {
    return (
      <p className="card px-4 py-5 text-center text-[12.5px] text-ink-muted">
        No meetings yet.
      </p>
    );
  }

  return (
    <ul className="card divide-y divide-hairline overflow-hidden">
      {meetings.map((meeting) => {
        const open = items.filter((item) => item.meeting_id === meeting.id && isOpen(item)).length;
        return (
          <li key={meeting.id} className="flex items-center gap-3 px-4 py-2.5">
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[13px] text-ink">{meeting.title}</span>
              <span className="block text-[11.5px] text-ink-faint">
                {new Date(meeting.started_at).toLocaleString([], {
                  day: "numeric",
                  month: "short",
                  hour: "numeric",
                  minute: "2-digit",
                })}
                {!meeting.ended_at && " · still open"}
              </span>
            </span>
            {open > 0 && (
              <span className="shrink-0 rounded-full bg-overlay px-2 py-0.5 text-[11px] text-ink-muted">
                {open} open
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}
