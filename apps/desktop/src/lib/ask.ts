import { useCallback, useState } from "react";

import { ApiError, type Citation, type GroundedAnswer } from "./api";
import type { Route } from "./router";
import { offered } from "./scope";

/**
 * Asking, in one place.
 *
 * Three surfaces ask the same kind of question — the Ask page, the panel that opens from any screen,
 * and a note's own Ask — and each used to keep its own copy of the conversation, its own citation
 * handling and its own wording for "nothing answers this". The copies drifted: the note's still told
 * people matching was by word after semantic search made that false. One implementation means the
 * three behave the same and differ only in where they search.
 */

export interface AskMessage {
  role: "user" | "assistant";
  content: string;
}

export interface AskTurn {
  question: string;
  /** `null` while the answer is on its way. */
  answer: GroundedAnswer | null;
}

export const SUGGESTIONS = [
  "What did we decide about pricing?",
  "What am I supposed to be doing this week?",
  "Who has raised concerns about the timeline?",
];

/**
 * Why an answer has no material behind it, in words a user can act on.
 *
 * Two different situations reach here, and they call for opposite responses: nothing matched
 * (other words may find it), or material was found and none of it answers the question (it is
 * not there to find).
 */
export function notGroundedNote(answer: GroundedAnswer): string {
  return (answer.considered ?? 0) > 0
    ? "Some of your material looked related, but none of it answers this."
    : "Nothing in your workspace matched this. Different wording may find it.";
}

/**
 * Where a citation leads, or `null` when this build has no page for it.
 *
 * A ticket can be cited — it is the user's material — while Tickets is held back; the citation then
 * shows and does not open. See `scope`.
 */
export function citationRoute(citation: Citation): Route | null {
  switch (citation.kind) {
    case "meeting":
      return { name: "meeting", id: citation.id, tab: "transcript" };
    case "note":
      return { name: "notes", id: citation.id };
    case "ticket":
      return offered("tickets") ? { name: "tickets" } : null;
  }
}

/** The thread so far as the engine wants it: questions and answer texts, never citation lists. */
export function threadMessages(turns: AskTurn[], next: string): AskMessage[] {
  return [
    ...turns.flatMap((turn): AskMessage[] =>
      turn.answer
        ? [
            { role: "user", content: turn.question },
            { role: "assistant", content: turn.answer.text },
          ]
        : [],
    ),
    { role: "user", content: next },
  ];
}

export interface AskThread {
  turns: AskTurn[];
  draft: string;
  setDraft: (draft: string) => void;
  busy: boolean;
  error: string | null;
  /** Ask `text`, or the draft when called with nothing. */
  send: (text?: string) => Promise<void>;
  /** Start over — a different note, or a closed panel opened fresh. */
  reset: () => void;
}

/**
 * One conversation with whatever `ask` reaches.
 *
 * The whole exchange goes with each question, so a follow-up like "and who owns that?" has something
 * to refer to. A question that fails is taken back out of the thread and returned to the box, so it
 * can be sent again without retyping and nothing sits there with no answer under it.
 */
export function useAskThread(ask: (messages: AskMessage[]) => Promise<GroundedAnswer>): AskThread {
  const [turns, setTurns] = useState<AskTurn[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const send = useCallback(
    async (text?: string) => {
      const question = (text ?? draft).trim();
      if (!question || busy) return;

      setBusy(true);
      setError(null);
      setDraft("");
      setTurns((current) => [...current, { question, answer: null }]);

      try {
        const answer = await ask(threadMessages(turns, question));
        setTurns((current) =>
          current.map((turn, i) => (i === current.length - 1 ? { ...turn, answer } : turn)),
        );
      } catch (e) {
        setTurns((current) => current.slice(0, -1));
        setDraft(question);
        setError(e instanceof ApiError ? e.message : "Could not get an answer.");
      } finally {
        setBusy(false);
      }
    },
    [ask, busy, draft, turns],
  );

  const reset = useCallback(() => {
    setTurns([]);
    setError(null);
    setDraft("");
  }, []);

  return { turns, draft, setDraft, busy, error, send, reset };
}
