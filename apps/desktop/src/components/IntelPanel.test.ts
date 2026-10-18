import { describe, expect, it } from "vitest";

import type { ClarifyingQuestion } from "../lib/api";
import { MAX_SHOWN, STALE_AFTER_MS, currentQuestions, questionKey } from "./IntelPanel";

function question(text: string, at_ms: number): ClarifyingQuestion {
  return { question: text, about: "", kind: "unassigned_action", at_ms };
}

describe("the questions the panel shows", () => {
  /**
   * The window appends every question it receives for the length of the recording. Rendered as
   * given, the newest question — the one about what was just said — sat at the bottom of an
   * ever-longer list.
   */
  it("puts the newest first and caps how many show", () => {
    const all = [10, 20, 30, 40, 50].map((n) => question(`Q${n}`, n * 1000));
    const seen = new Map(all.map((q) => [questionKey(q), 0]));

    const shown = currentQuestions(all, seen, 1_000);

    expect(shown.map((q) => q.question)).toEqual(["Q50", "Q40", "Q30"]);
    expect(shown).toHaveLength(MAX_SHOWN);
  });

  it("drops a question once it has been up for five minutes", () => {
    const old = question("Who owns the backfill?", 0);
    const fresh = question("Which budget holds?", 280_000);
    const seen = new Map([
      [questionKey(old), 0],
      [questionKey(fresh), 290_000],
    ]);

    expect(currentQuestions([old, fresh], seen, STALE_AFTER_MS - 1)).toHaveLength(2);
    expect(currentQuestions([old, fresh], seen, STALE_AFTER_MS + 1)).toEqual([fresh]);
  });

  it("treats a question it has not seen before as just arrived", () => {
    expect(currentQuestions([question("New?", 5)], new Map(), 10_000_000)).toHaveLength(1);
  });

  it("does not reorder the caller's list", () => {
    const all = [question("A", 1), question("B", 2)];
    currentQuestions(all, new Map(), 0);
    expect(all.map((q) => q.question)).toEqual(["A", "B"]);
  });
});
