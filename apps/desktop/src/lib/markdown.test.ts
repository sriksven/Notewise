import { describe, expect, it } from "vitest";

import { parseInline, parseMarkdown } from "./markdown";

describe("parseInline", () => {
  it("splits bold runs out of the surrounding text", () => {
    expect(parseInline("keep the **read replica** for now")).toEqual([
      { text: "keep the ", bold: false },
      { text: "read replica", bold: true },
      { text: " for now", bold: false },
    ]);
  });

  // An unterminated `**` is what a truncated generation looks like. Treating it as the start of
  // emphasis would bold the entire rest of the summary.
  it("leaves an unclosed marker as text", () => {
    expect(parseInline("a **dangling marker")).toEqual([
      { text: "a **dangling marker", bold: false },
    ]);
  });

  it("returns plain text unchanged", () => {
    expect(parseInline("nothing special")).toEqual([{ text: "nothing special", bold: false }]);
  });
});

describe("parseMarkdown", () => {
  // Exactly what llama3.1 produced for a real meeting. If this renders as literal asterisks the
  // summary screen looks broken, which is the whole reason this parser exists.
  it("reads a summary the way a local model actually writes one", () => {
    const blocks = parseMarkdown(
      "**Outcome:** The team decided to split the analytics workload.\n" +
        "\n" +
        "**Decisions:**\n" +
        "\n" +
        "* Splitting prevents connection limits.\n" +
        "* Ana owns the plan.\n",
    );

    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "heading", "list"]);
    expect(blocks[1]).toEqual({ kind: "heading", spans: [{ text: "Decisions", bold: false }] });
    expect(blocks[2]).toEqual({
      kind: "list",
      items: [
        [{ text: "Splitting prevents connection limits.", bold: false }],
        [{ text: "Ana owns the plan.", bold: false }],
      ],
    });
  });

  it("keeps bold inside a paragraph rather than promoting it to a heading", () => {
    const blocks = parseMarkdown("**Outcome:** we split the database.");
    expect(blocks).toEqual([
      {
        kind: "paragraph",
        spans: [
          { text: "Outcome:", bold: true },
          { text: " we split the database.", bold: false },
        ],
      },
    ]);
  });

  it("handles hash headings and numbered lists", () => {
    const blocks = parseMarkdown("## Next steps\n1. Write the plan\n2. Review it");
    expect(blocks).toEqual([
      { kind: "heading", spans: [{ text: "Next steps", bold: false }] },
      {
        kind: "list",
        items: [
          [{ text: "Write the plan", bold: false }],
          [{ text: "Review it", bold: false }],
        ],
      },
    ]);
  });

  it("joins wrapped lines into one paragraph", () => {
    const blocks = parseMarkdown("The team met\nand agreed to split.");
    expect(blocks).toEqual([
      { kind: "paragraph", spans: [{ text: "The team met and agreed to split.", bold: false }] },
    ]);
  });

  it("is empty for empty input", () => {
    expect(parseMarkdown("")).toEqual([]);
  });
});
