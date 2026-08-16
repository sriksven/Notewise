import { describe, expect, it } from "vitest";

import { hasFormatting, markForKey, parseInline, toggleMark, type Mark } from "./inline";

describe("parseInline", () => {
  it("leaves plain text alone", () => {
    expect(parseInline("just words")).toEqual([{ text: "just words", marks: [] }]);
  });

  it("reads each mark", () => {
    expect(parseInline("**bold**")).toEqual([{ text: "bold", marks: ["bold"] }]);
    expect(parseInline("*italic*")).toEqual([{ text: "italic", marks: ["italic"] }]);
    expect(parseInline("_italic_")).toEqual([{ text: "italic", marks: ["italic"] }]);
    expect(parseInline("`code`")).toEqual([{ text: "code", marks: ["code"] }]);
    expect(parseInline("~~gone~~")).toEqual([{ text: "gone", marks: ["strike"] }]);
  });

  it("keeps the text around a mark", () => {
    expect(parseInline("a **b** c")).toEqual([
      { text: "a ", marks: [] },
      { text: "b", marks: ["bold"] },
      { text: " c", marks: [] },
    ]);
  });

  // `**` has to be tried before `*`, or bold parses as two empty italics.
  it("prefers bold over italic", () => {
    expect(parseInline("**strong**")).toEqual([{ text: "strong", marks: ["bold"] }]);
  });

  it("nests marks", () => {
    expect(parseInline("**bold and *italic* too**")).toEqual([
      { text: "bold and ", marks: ["bold"] },
      { text: "italic", marks: ["bold", "italic"] },
      { text: " too", marks: ["bold"] },
    ]);
  });

  // Code is opaque: `**not bold**` inside backticks is four literal characters and a word.
  it("does not format inside code", () => {
    expect(parseInline("`**not bold**`")).toEqual([
      { text: "**not bold**", marks: ["code"] },
    ]);
  });

  // An unclosed delimiter is text, not emphasis that never ends.
  it("treats an unclosed marker as literal", () => {
    expect(parseInline("2 * 3 = 6")).toEqual([{ text: "2 * 3 = 6", marks: [] }]);
    expect(parseInline("**unclosed")).toEqual([{ text: "**unclosed", marks: [] }]);
    expect(parseInline("a `b")).toEqual([{ text: "a `b", marks: [] }]);
  });

  it("treats an empty marker pair as literal", () => {
    expect(parseInline("****")).toEqual([{ text: "****", marks: [] }]);
    expect(parseInline("a ** b")).toEqual([{ text: "a ** b", marks: [] }]);
  });

  it("reads a link", () => {
    expect(parseInline("see [the docs](https://example.com)")).toEqual([
      { text: "see ", marks: [] },
      { text: "the docs", marks: [], href: "https://example.com" },
    ]);
  });

  it("falls back to the url when a link has no text", () => {
    expect(parseInline("[](https://example.com)")).toEqual([
      { text: "https://example.com", marks: [], href: "https://example.com" },
    ]);
  });

  it("leaves bracket text that is not a link alone", () => {
    expect(parseInline("[not a link]")).toEqual([{ text: "[not a link]", marks: [] }]);
    expect(parseInline("array[0] = 1")).toEqual([{ text: "array[0] = 1", marks: [] }]);
  });

  it("handles several marks in one line", () => {
    expect(parseInline("**a** and `b` and ~~c~~")).toEqual([
      { text: "a", marks: ["bold"] },
      { text: " and ", marks: [] },
      { text: "b", marks: ["code"] },
      { text: " and ", marks: [] },
      { text: "c", marks: ["strike"] },
    ]);
  });

  it("returns nothing for an empty line", () => {
    expect(parseInline("")).toEqual([]);
  });
});

describe("hasFormatting", () => {
  // Drives whether a block renders as spans or as its raw text, so it has to agree with the
  // parser exactly.
  it("is true only when something would render differently", () => {
    expect(hasFormatting("**bold**")).toBe(true);
    expect(hasFormatting("[a](b)")).toBe(true);
    expect(hasFormatting("plain")).toBe(false);
    expect(hasFormatting("2 * 3")).toBe(false);
    expect(hasFormatting("")).toBe(false);
  });
});

describe("toggleMark", () => {
  const sel = (text: string, start: number, end: number) => ({ text, start, end });

  it("wraps a selection", () => {
    expect(toggleMark(sel("hello world", 0, 5), "bold")).toEqual({
      text: "**hello** world",
      start: 2,
      end: 7,
    });
  });

  it("keeps the same words selected afterwards", () => {
    const result = toggleMark(sel("hello world", 6, 11), "italic");
    expect(result.text.slice(result.start, result.end)).toBe("world");
  });

  // An editor where ⌘B only ever adds markers accumulates `****bold****`.
  it("unwraps when the markers are inside the selection", () => {
    expect(toggleMark(sel("**bold** text", 0, 8), "bold")).toEqual({
      text: "bold text",
      start: 0,
      end: 4,
    });
  });

  it("unwraps when the markers are just outside the selection", () => {
    // `bold` selected, with `**` on either side.
    expect(toggleMark(sel("**bold** text", 2, 6), "bold")).toEqual({
      text: "bold text",
      start: 0,
      end: 4,
    });
  });

  it("round-trips", () => {
    const original = sel("make this bold", 10, 14);
    const bolded = toggleMark(original, "bold");
    const back = toggleMark(bolded, "bold");
    expect(back.text).toBe(original.text);
  });

  // ⌘B with nothing selected then typing should produce bold text.
  it("inserts an empty pair and puts the caret inside", () => {
    const result = toggleMark(sel("ab", 1, 1), "bold");
    expect(result.text).toBe("a****b");
    expect(result.start).toBe(3);
    expect(result.end).toBe(3);
  });

  it("uses the right marker per mark", () => {
    const cases: Array<[Mark, string]> = [
      ["bold", "**x**"],
      ["italic", "*x*"],
      ["code", "`x`"],
      ["strike", "~~x~~"],
    ];
    for (const [mark, expected] of cases) {
      expect(toggleMark(sel("x", 0, 1), mark).text, mark).toBe(expected);
    }
  });

  it("does not confuse italic markers with the ends of bold ones", () => {
    // `**x**` with `x` selected: toggling italic must wrap rather than unwrap the bold.
    const result = toggleMark(sel("**x**", 2, 3), "italic");
    expect(result.text).toBe("***x***");
  });
});

describe("markForKey", () => {
  it("maps the chords it claims", () => {
    expect(markForKey("b", false)).toBe("bold");
    expect(markForKey("i", false)).toBe("italic");
    expect(markForKey("e", false)).toBe("code");
    expect(markForKey("x", true)).toBe("strike");
  });

  it("is case-insensitive", () => {
    expect(markForKey("B", false)).toBe("bold");
  });

  it("claims nothing else", () => {
    // ⌘K is the app's search shortcut and must reach it.
    expect(markForKey("k", false)).toBeNull();
    expect(markForKey("n", false)).toBeNull();
    expect(markForKey("x", false)).toBeNull();
    expect(markForKey("b", true)).toBeNull();
  });
});
