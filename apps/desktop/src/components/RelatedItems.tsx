import { useEffect, useState } from "react";
import { FileText, Link2, TicketCheck, Users, Video } from "lucide-react";

import { api, type RelatedNode } from "../lib/api";
import type { Route } from "../lib/router";

interface Props {
  meetingId: string;
  onNavigate: (route: Route) => void;
}

/** Only the kinds a person can open. Anything else has no label and is not shown. */
const ICONS: Record<string, typeof FileText> = {
  meeting: Video,
  note: FileText,
  ticket: TicketCheck,
  person: Users,
};

/**
 * What else this meeting is connected to.
 *
 * # Why this is the graph and not a list of links
 *
 * The whole premise of the object graph is that a meeting produces notes, tickets and commitments
 * and that those stay attached to it. Until now nothing rendered a single edge: the graph was real,
 * queried by retrieval and by the agent, and invisible. This is the smallest honest view of it —
 * what is attached, how far away, and by which edge.
 *
 * # Only what can be named
 *
 * The endpoint returns every node within the depth, including transcript segments and summaries that
 * have no name of their own and are not anywhere to go. Those come back with a null label and are
 * skipped rather than rendered as "summary (1 hop)", which tells a reader nothing they can act on.
 *
 * # Why the edge is shown
 *
 * "via mentions" versus "via became_ticket" is the difference between something that referred to the
 * meeting and something the meeting produced. That distinction is the reason edges are typed, and
 * hiding it would make every relationship look the same.
 */
export function RelatedItems({ meetingId, onNavigate }: Props) {
  const [nodes, setNodes] = useState<RelatedNode[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    setNodes(null);
    void api
      .related(meetingId)
      .then((found) => !cancelled && setNodes(found))
      .catch(() => {
        // A graph query that will not load should not take the panel with it.
        if (!cancelled) setNodes([]);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  if (nodes === null) {
    return <p className="text-[12px] leading-relaxed text-ink-faint">Loading…</p>;
  }

  const shown = nodes.filter((node) => node.label && ICONS[node.kind]);

  if (shown.length === 0) {
    return (
      <p className="text-[12px] leading-relaxed text-ink-faint">
        Nothing linked yet. Notes written about this meeting, and tickets made from its action items,
        appear here.
      </p>
    );
  }

  const open = (node: RelatedNode) => {
    switch (node.kind) {
      case "meeting":
        return onNavigate({ name: "meeting", id: node.id, tab: "transcript" });
      case "note":
        return onNavigate({ name: "notes", id: node.id });
      case "person":
        return onNavigate({ name: "people", id: node.id });
      case "ticket":
        // Tickets have no per-ticket address, so this lands on the board that holds it.
        return onNavigate({ name: "tickets" });
    }
  };

  return (
    <ul className="space-y-1">
      {shown.map((node) => {
        const Icon = ICONS[node.kind] ?? Link2;
        return (
          <li key={`${node.kind}-${node.id}`}>
            <button
              type="button"
              onClick={() => open(node)}
              className="flex w-full items-start gap-2 rounded-lg px-2 py-1.5 text-left transition
                         hover:bg-overlay"
            >
              <Icon size={12} className="mt-0.5 shrink-0 text-ink-faint" aria-hidden />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[12.5px] text-ink">{node.label}</span>
                <span className="text-[11px] text-ink-faint">
                  {node.kind} · via {node.via.replace(/_/g, " ")}
                </span>
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}
