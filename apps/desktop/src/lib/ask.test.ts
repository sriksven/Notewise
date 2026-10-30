import { describe, expect, it } from "vitest";

import type { Citation, GroundedAnswer } from "./api";
import { citationRoute, threadMessages, type AskTurn } from "./ask";

function cite(kind: Citation["kind"]): Citation {
  return { n: 1, kind, id: "x1", title: "Pricing review" };
}

function answered(question: string, text: string): AskTurn {
  const answer: GroundedAnswer = { text, model: "m", citations: [cite("note")], grounded: true };
  return { question, answer };
}

describe("citationRoute", () => {
  it("opens a meeting at its transcript, where the cited words are", () => {
    expect(citationRoute(cite("meeting"))).toEqual({ name: "meeting", id: "x1", tab: "transcript" });
  });

  it("opens a note", () => {
    expect(citationRoute(cite("note"))).toEqual({ name: "notes", id: "x1" });
  });

  // Tickets is held back in this build, so a cited ticket shows and does not open a page that
  // would only redirect home.
  it("has nowhere to send a ticket while Tickets is held back", () => {
    expect(citationRoute(cite("ticket"))).toBeNull();
  });
});

describe("threadMessages", () => {
  it("sends the conversation so far, then the new question", () => {
    const messages = threadMessages([answered("What did we decide?", "Pricing stays.")], "Who owns it?");
    expect(messages).toEqual([
      { role: "user", content: "What did we decide?" },
      { role: "assistant", content: "Pricing stays." },
      { role: "user", content: "Who owns it?" },
    ]);
  });

  // A question still waiting for its answer would otherwise go out with no reply after it,
  // and the model would read two questions in a row as one.
  it("leaves out a question that has no answer yet", () => {
    const messages = threadMessages([{ question: "Pending?", answer: null }], "Next");
    expect(messages).toEqual([{ role: "user", content: "Next" }]);
  });
});
