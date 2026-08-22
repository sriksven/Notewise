import { useCallback, useEffect, useState } from "react";
import { Plus, User, X } from "lucide-react";

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

  /**
   * Delete a ticket.
   *
   * Asks first, unlike removing an action item. A ticket is deliberately created work with a status
   * someone has been moving along, there is no trash to recover it from, and the card it lives on is
   * a click target for advancing status — so a delete control beside that is one slip away from
   * being pressed by mistake.
   */
  async function remove(ticket: Ticket) {
    const ok = window.confirm(
      `Delete "${ticket.title}"?\n\nThere is no undo. Any action item it came from stays.`,
    );
    if (!ok) return;

    const before = tickets;
    setTickets((current) => current.filter((t) => t.id !== ticket.id));
    try {
      await api.deleteTicket(ticket.id);
      setError(null);
    } catch (e) {
      // Already deleted is the outcome that was wanted; see the note in `ActionItems`.
      if (e instanceof ApiError && e.status === 404) return;
      setTickets(before);
      setError(e instanceof ApiError ? e.message : "Could not delete that ticket.");
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
        <h2 className="text-[13px] font-semibold text-ink">Tickets</h2>
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="flex items-center gap-1 rounded-md border border-hairline bg-surface px-2
                     py-1 text-[11.5px] font-medium text-ink-muted transition
                     hover:bg-overlay"
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
            className="w-full rounded-md border border-hairline bg-surface px-2 py-1.5
                       text-[13px] text-ink outline-none
                       placeholder:text-ink-faint focus:border-hairline"
          />
        </form>
      )}

      {error && (
        <p role="status" className="px-5 py-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}

      {loading && tickets.length === 0 ? (
        <p className="px-5 py-4 text-[12.5px] text-ink-faint">Loading…</p>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-3 gap-4 overflow-y-auto px-5 py-4">
          {COLUMNS.map((column) => {
            const inColumn = tickets.filter((t) => t.status === column.status);
            return (
              <section key={column.status} className="min-w-0">
                <h3 className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-ink-faint">
                  {column.label}
                  {inColumn.length > 0 && (
                    <span className="rounded-full bg-overlay px-1.5 py-px text-[10px] tabular-nums text-ink-muted">
                      {inColumn.length}
                    </span>
                  )}
                </h3>

                {inColumn.length === 0 ? (
                  <p className="text-[11.5px] text-ink-faint">Nothing here.</p>
                ) : (
                  <ul className="space-y-1.5">
                    {inColumn.map((ticket) => (
                      <li key={ticket.id} className="group relative">
                        <button
                          type="button"
                          onClick={() => void move(ticket)}
                          title={`Move to ${
                            COLUMNS.find((c) => c.status === advance(ticket.status))?.label
                          }`}
                          className="w-full rounded-lg border border-hairline bg-surface p-2.5
                                     text-left transition hover:border-hairline"
                        >
                          <p
                            className={`text-[12.5px] leading-snug ${
                              ticket.status === "done"
                                ? "text-ink-faint line-through"
                                : "text-ink"
                            }`}
                          >
                            {ticket.title}
                          </p>
                          <span
                            className={`mt-1 inline-flex items-center gap-0.5 text-[11px] ${
                              ticket.owner ? "text-ink-muted" : "text-warn-text"
                            }`}
                          >
                            <User size={10} aria-hidden />
                            {ticket.owner ?? "unassigned"}
                          </span>
                        </button>

                        {/* Outside the card, not inside it: the card is a button, and nesting one
                            button in another is invalid HTML. Positioned over the corner so it
                            reads as belonging to the card without being part of its click target. */}
                        <button
                          type="button"
                          onClick={() => void remove(ticket)}
                          title="Delete this ticket"
                          aria-label={`Delete: ${ticket.title}`}
                          className="absolute right-1 top-1 rounded p-0.5 text-ink-faint opacity-0
                                     transition hover:bg-overlay hover:text-warn-text
                                     group-hover:opacity-100 focus-visible:opacity-100"
                        >
                          <X size={12} aria-hidden />
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
