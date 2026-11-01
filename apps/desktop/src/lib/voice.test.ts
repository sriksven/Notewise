import { describe, expect, it } from "vitest";

import { appendSpoken, joinHeard, spaceBefore } from "./voice";

describe("joinHeard", () => {
  it("puts the guess after the kept words", () => {
    expect(joinHeard("What did we decide?", "and who owns")).toBe(
      "What did we decide? and who owns",
    );
  });

  it("shows only what there is", () => {
    expect(joinHeard("", "what did")).toBe("what did");
    expect(joinHeard("What did we decide?", "")).toBe("What did we decide?");
    expect(joinHeard("", "")).toBe("");
  });

  it("never doubles a space Whisper left at either edge", () => {
    expect(joinHeard(" kept ", " guess ")).toBe("kept guess");
  });
});

describe("spaceBefore", () => {
  it("adds a space only after a word", () => {
    expect(spaceBefore("")).toBe("");
    expect(spaceBefore("Summarise")).toBe(" ");
    expect(spaceBefore("Summarise ")).toBe("");
    expect(spaceBefore("Line one\n")).toBe("");
  });
});

describe("appendSpoken", () => {
  it("fills an empty box", () => {
    expect(appendSpoken("", "What did we decide?")).toBe("What did we decide?");
  });

  it("keeps what was typed and adds what was said after it", () => {
    expect(appendSpoken("In the launch meeting,", "who owns rollback?")).toBe(
      "In the launch meeting, who owns rollback?",
    );
    expect(appendSpoken("In the launch meeting, ", "who owns rollback?")).toBe(
      "In the launch meeting, who owns rollback?",
    );
  });

  it("leaves the box alone when nothing was said", () => {
    expect(appendSpoken("Typed", "   ")).toBe("Typed");
  });

  it("trims what Whisper hands back", () => {
    expect(appendSpoken("", "  Who owns it?  ")).toBe("Who owns it?");
  });
});
