import { useCallback, useEffect, useState } from "react";
import { Plus, User } from "lucide-react";

import { api, ApiError, type Ticket } from "../lib/api";

const COLUMNS: Array<{ status: string; label: string }> = [
  { status: "todo", label: "To do" },
  { status: "in_progress", label: "In progress" },
  { status: "done", label: "Done" },
];

/** The next status a click should move a ticket to, cycling through the board. */
function advance(status: string): string {
  const index = COLUMNS.findIndex((c) => c.status === status);
  return COLUMNS[(index + 1) % COLUMNS.length]?.status ?? "todo";
}

/**
 * Native tickets — the work a meeting produced, tracked without an external tracker.
 *
 * The roadmap's bet is that the free local product should be useful with nothing connected,
 * which means tickets have to live here before they can be pushed anywhere. Pushing to Linear
 * or Jira is a later phase and deliberately one-way when it lands.
 *
 * A board rather than a list because status is the thing a user scans for. Columns are driven
 * by `COLUMNS` rather than by whatever statuses happen to exist, so an empty column still
 * appears — "nothing in progress" is information, and a column that vanishes when it empties
 * makes the board jump around as work moves.
 */
export function TicketsView() {
  const [tickets, setTickets] = useState<Ticket[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [adding, setAdding] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setTickets(await api.tickets());
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not load tickets.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function move(ticket: Ticket) {
    const next = advance(ticket.status);
    const before = tickets;

    setTickets((current) =>
      current.map((t) => (t.id === ticket.id ? { ...t, status: next } : t)),
    );
    try {
      await api.updateTicket(ticket.id, { status: next });
    } catch (e) {
      setTickets(before);
      setError(e instanceof ApiError ? e.message : "Could not move that ticket.");
    }
  }

  async function add() {
    const title = draft.trim();
    if (!title) return;

    setDraft("");
    setAdding(false);
    try {
      const created = await api.createTicket({ title });
      setTickets((current) => [...current, created]);
      setError(null);
    } catch (e) {
      setDraft(title);
      setAdding(true);
      setError(e instanceof ApiError ? e.message : "Could not create that ticket.");
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center justify-between border-b border-hairline px-5 py-3">
        <h2 className="text-[13px] font-semibold text-neutral-800">Tickets</h2>
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="flex items-center gap-1 rounded-md border border-hairline bg-white px-2
                     py-1 text-[11.5px] font-medium text-neutral-600 transition
                     hover:bg-neutral-50"
        >
          <Plus size={11} aria-hidden />
          New ticket
        </button>
      </div>

      {adding && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void add();
          }}
          className="border-b border-hairline px-5 py-2"
        >
          <input
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => {
              if (!draft.trim()) setAdding(false);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setDraft("");
                setAdding(false);
              }
            }}
            placeholder="What needs doing?"
            aria-label="New ticket title"
            className="w-full rounded-md border border-hairline bg-white px-2 py-1.5
                       text-[13px] text-neutral-800 outline-none
                       placeholder:text-neutral-300 focus:border-neutral-300"
          />
        </form>
      )}

      {error && (
        <p role="status" className="px-5 py-2 text-[12px] text-red-600">
          {error}
        </p>
      )}

      {loading && tickets.length === 0 ? (
        <p className="px-5 py-4 text-[12.5px] text-neutral-400">Loading…</p>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-3 gap-4 overflow-y-auto px-5 py-4">
          {COLUMNS.map((column) => {
            const inColumn = tickets.filter((t) => t.status === column.status);
            return (
              <section key={column.status} className="min-w-0">
                <h3 className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-neutral-400">
                  {column.label}
                  {inColumn.length > 0 && (
                    <span className="rounded-full bg-neutral-100 px-1.5 py-px text-[10px] tabular-nums text-neutral-500">
                      {inColumn.length}
                    </span>
                  )}
                </h3>

                {inColumn.length === 0 ? (
                  <p className="text-[11.5px] text-neutral-300">Nothing here.</p>
                ) : (
                  <ul className="space-y-1.5">
                    {inColumn.map((ticket) => (
                      <li key={ticket.id}>
                        <button
                          type="button"
                          onClick={() => void move(ticket)}
                          title={`Move to ${
                            COLUMNS.find((c) => c.status === advance(ticket.status))?.label
                          }`}
                          className="w-full rounded-lg border border-hairline bg-white p-2.5
                                     text-left transition hover:border-neutral-300"
                        >
                          <p
                            className={`text-[12.5px] leading-snug ${
                              ticket.status === "done"
                                ? "text-neutral-400 line-through"
                                : "text-neutral-800"
                            }`}
                          >
                            {ticket.title}
                          </p>
                          <span
                            className={`mt-1 inline-flex items-center gap-0.5 text-[11px] ${
                              ticket.owner ? "text-neutral-500" : "text-amber-700"
                            }`}
                          >
                            <User size={10} aria-hidden />
                            {ticket.owner ?? "unassigned"}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}
