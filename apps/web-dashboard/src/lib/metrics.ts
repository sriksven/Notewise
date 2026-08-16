/**
 * Turning a workspace into numbers.
 *
 * Pure functions over data already fetched, so every one of them is testable without a
 * network — which matters more here than usual, because a dashboard's whole job is to be
 * *correct* about counts. A view that renders beautifully and says 11 when the answer is 12 is
 * worse than no view.
 */

import type { ActionItem, Meeting, Note, Ticket } from "./api";

/** Work that is neither finished nor abandoned. */
export function isOpen(item: { status?: string }): boolean {
  return item.status !== "done" && item.status !== "cancelled";
}

export function isOverdue(item: { due_at: string | null; status?: string }, now = new Date()): boolean {
  if (!item.due_at || !isOpen(item)) return false;
  const due = new Date(item.due_at);
  return !Number.isNaN(due.getTime()) && due.getTime() < now.getTime();
}

/** Wall-clock length of a meeting in minutes, or 0 for one that never ended. */
export function durationMinutes(meeting: Meeting): number {
  if (!meeting.ended_at) return 0;
  const start = new Date(meeting.started_at).getTime();
  const end = new Date(meeting.ended_at).getTime();
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) return 0;
  return Math.round((end - start) / 60_000);
}

export interface Totals {
  meetings: number;
  /** Total recorded time, in minutes. Excludes meetings with no end. */
  minutes: number;
  notes: number;
  openWork: number;
  overdue: number;
  decisions: number;
}

export function totals(input: {
  meetings: Meeting[];
  notes: Note[];
  actionItems: ActionItem[];
  tickets: Ticket[];
  decisions: number;
}): Totals {
  const work = [...input.actionItems, ...input.tickets];
  return {
    meetings: input.meetings.length,
    minutes: input.meetings.reduce((sum, meeting) => sum + durationMinutes(meeting), 0),
    notes: input.notes.length,
    openWork: work.filter(isOpen).length,
    overdue: work.filter((item) => isOverdue(item)).length,
    decisions: input.decisions,
  };
}

export interface DayCount {
  /** `YYYY-MM-DD`, in local time. */
  day: string;
  count: number;
}

/**
 * Meetings per day over the last `days`, oldest first.
 *
 * Every day in the window appears, including the empty ones. A bar chart that silently omits
 * quiet days compresses the axis and turns a fortnight of nothing into a flat busy line — the
 * gaps are the information.
 *
 * Local dates, not UTC: a meeting at 9pm on Tuesday belongs to Tuesday for the person who was
 * in it, whatever that is in UTC.
 */
export function meetingsPerDay(meetings: Meeting[], days = 30, now = new Date()): DayCount[] {
  const counts = new Map<string, number>();

  for (let back = days - 1; back >= 0; back -= 1) {
    const date = new Date(now.getFullYear(), now.getMonth(), now.getDate() - back);
    counts.set(localDay(date), 0);
  }

  for (const meeting of meetings) {
    const date = new Date(meeting.started_at);
    if (Number.isNaN(date.getTime())) continue;
    const key = localDay(date);
    // Only within the window; older meetings are counted in the totals, not the chart.
    if (counts.has(key)) counts.set(key, (counts.get(key) ?? 0) + 1);
  }

  return [...counts].map(([day, count]) => ({ day, count }));
}

function localDay(date: Date): string {
  const month = `${date.getMonth() + 1}`.padStart(2, "0");
  const day = `${date.getDate()}`.padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

export interface OwnerLoad {
  owner: string;
  open: number;
  overdue: number;
}

/**
 * Open work grouped by who owns it, busiest first.
 *
 * Unowned work is its own row rather than being dropped. It is usually the largest one, and it
 * is the row that matters — an action item nobody owns is the one that does not get done.
 */
export function loadByOwner(items: Array<ActionItem | Ticket>): OwnerLoad[] {
  const open = items.filter(isOpen);
  const byOwner = new Map<string, OwnerLoad>();

  for (const item of open) {
    const owner = item.owner?.trim() || "Unassigned";
    const entry = byOwner.get(owner) ?? { owner, open: 0, overdue: 0 };
    entry.open += 1;
    if (isOverdue(item)) entry.overdue += 1;
    byOwner.set(owner, entry);
  }

  return [...byOwner.values()].sort(
    // Overdue breaks a tie on volume: two people with four items each are not equally stuck.
    (a, b) => b.open - a.open || b.overdue - a.overdue || a.owner.localeCompare(b.owner),
  );
}

/** A duration in minutes as a person would say it. */
export function readableMinutes(minutes: number): string {
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}
