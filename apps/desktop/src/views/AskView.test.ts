import { describe, expect, it } from "vitest";

import type { GroundedAnswer } from "../lib/api";
import { notGroundedNote } from "./AskView";

function refusal(considered?: number): GroundedAnswer {
  return { text: "", model: "m", citations: [], grounded: false, considered };
}

describe("the note under an answer with no material", () => {
  it("suggests other words only when nothing matched", () => {
    expect(notGroundedNote(refusal(0))).toMatch(/Nothing in your workspace matched/);
    expect(notGroundedNote(refusal(undefined))).toMatch(/Nothing in your workspace matched/);
  });

  /**
   * The engine had just said "Looked at 3 passages and none of them answer that question", and
   * this used to sit above it saying nothing matched.
   */
  it("says the material is not there when passages were found and none answered", () => {
    const note = notGroundedNote(refusal(3));
    expect(note).toMatch(/none of it answers this/);
    expect(note).not.toMatch(/Nothing in your workspace matched/);
  });

  it("never claims retrieval is by word", () => {
    for (const considered of [0, 3]) {
      expect(notGroundedNote(refusal(considered))).not.toMatch(/by word/);
    }
  });
});
