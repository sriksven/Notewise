import { describe, expect, it } from "vitest";

import {
  MAX_DEPTH,
  blockAfter,
  canIndent,
  canOutdent,
  depthOf,
  emptyDocument,
  imageParts,
  imageText,
  indent,
  isListItem,
  newBlock,
  newImage,
  newTable,
  outdent,
  parse,
  serialize,
  shortcutFor,
  tableRows,
  tableText,
  toPlainText,
  type Block,
  type BlockType,
} from "./blocks";

/** Blocks without their ids, which are not persisted and not part of the document. */
function shape(blocks: Block[]): Array<Omit<Block, "id">> {
  return blocks.map(({ id: _id, ...rest }) => rest);
}

describe("parse", () => {
  it("reads a plain paragraph", () => {
    expect(shape(parse("Just some text."))).toEqual([
      { type: "paragraph", text: "Just some text." },
    ]);
  });

  it("reads the three heading levels", () => {
    expect(shape(parse("# One\n\n## Two\n\n### Three"))).toEqual([
      { type: "heading1", text: "One" },
      { type: "heading2", text: "Two" },
      { type: "heading3", text: "Three" },
    ]);
  });

  it("reads bullets written with either marker", () => {
    expect(shape(parse("- dash\n* star"))).toEqual([
      { type: "bullet", text: "dash" },
      { type: "bullet", text: "star" },
    ]);
  });

  // Ordering in the rule table: the todo forms have to be tried before the plain bullet, or
  // `- [ ] thing` parses as a bullet whose text is `[ ] thing`.
  it("reads checked and unchecked todos", () => {
    expect(shape(parse("- [ ] open\n- [x] done\n- [X] also done"))).toEqual([
      { type: "todo", text: "open", checked: false },
      { type: "todo", text: "done", checked: true },
      { type: "todo", text: "also done", checked: true },
    ]);
  });

  it("reads numbered items written with a dot or a bracket", () => {
    expect(shape(parse("1. first\n2) second"))).toEqual([
      { type: "numbered", text: "first" },
      { type: "numbered", text: "second" },
    ]);
  });

  it("reads quotes with or without a space", () => {
    expect(shape(parse("> spaced\n>tight"))).toEqual([
      { type: "quote", text: "spaced" },
      { type: "quote", text: "tight" },
    ]);
  });

  it("reads a fenced code block whole", () => {
    const blocks = parse("```\nlet x = 1;\nlet y = 2;\n```");
    expect(shape(blocks)).toEqual([{ type: "code", text: "let x = 1;\nlet y = 2;" }]);
  });

  // A `# comment` in a shell snippet is a comment, not a heading.
  it("does not parse markers inside code", () => {
    const blocks = parse("```\n# not a heading\n- not a bullet\n```");
    expect(blocks).toHaveLength(1);
    expect(blocks[0].type).toBe("code");
    expect(blocks[0].text).toBe("# not a heading\n- not a bullet");
  });

  it("closes an unterminated code fence at the end of input", () => {
    const blocks = parse("```\nno closing fence");
    expect(shape(blocks)).toEqual([{ type: "code", text: "no closing fence" }]);
  });

  it("reads the divider forms", () => {
    for (const line of ["---", "***", "___", "- - -"]) {
      expect(shape(parse(line))).toEqual([{ type: "divider", text: "" }]);
    }
  });

  it("treats blank lines as separators rather than blocks", () => {
    expect(parse("one\n\n\n\ntwo")).toHaveLength(2);
  });

  // Every note written before the block editor existed is plain text.
  it("opens a document written as plain prose", () => {
    const legacy = "Some thoughts.\n\nAnd a second paragraph.";
    expect(shape(parse(legacy))).toEqual([
      { type: "paragraph", text: "Some thoughts." },
      { type: "paragraph", text: "And a second paragraph." },
    ]);
  });

  it("gives an empty document one block to type into", () => {
    for (const input of ["", "   ", "\n\n"]) {
      const blocks = parse(input);
      expect(blocks).toHaveLength(1);
      expect(blocks[0].type).toBe("paragraph");
      expect(blocks[0].text).toBe("");
    }
  });

  it("normalises Windows line endings", () => {
    expect(shape(parse("one\r\ntwo"))).toEqual([
      { type: "paragraph", text: "one" },
      { type: "paragraph", text: "two" },
    ]);
  });

  it("leaves inline formatting as literal text", () => {
    // The editor is a block parser; what is inside a line is the user's business.
    expect(shape(parse("some **bold** and `code`"))).toEqual([
      { type: "paragraph", text: "some **bold** and `code`" },
    ]);
  });

  it("gives every block a distinct id", () => {
    const blocks = parse("one\n\ntwo\n\nthree");
    expect(new Set(blocks.map((b) => b.id)).size).toBe(3);
  });
});

describe("serialize", () => {
  it("writes each type with its marker", () => {
    const blocks: Block[] = [
      newBlock("heading1", "Title"),
      newBlock("paragraph", "Text."),
      newBlock("bullet", "point"),
      newBlock("quote", "said"),
    ];
    expect(serialize(blocks)).toBe("# Title\n\nText.\n\n- point\n\n> said\n");
  });

  it("writes a todo's state", () => {
    const open = newBlock("todo", "open");
    const done: Block = { ...newBlock("todo", "done"), checked: true };
    expect(serialize([open, done])).toBe("- [ ] open\n- [x] done\n");
  });

  // A blank line between list items breaks the list in most renderers.
  it("keeps consecutive list items adjacent", () => {
    const blocks = [newBlock("bullet", "a"), newBlock("bullet", "b"), newBlock("bullet", "c")];
    expect(serialize(blocks)).toBe("- a\n- b\n- c\n");
  });

  it("separates blocks of different types", () => {
    const blocks = [newBlock("bullet", "a"), newBlock("paragraph", "b")];
    expect(serialize(blocks)).toBe("- a\n\nb\n");
  });

  /**
   * Renumbered from position rather than preserving what was typed: a reordered list should
   * read 1, 2, 3, and every Markdown renderer renumbers anyway — keeping the original digits
   * would make the file disagree with every view of it.
   */
  it("renumbers a numbered list from its position", () => {
    const blocks = [
      newBlock("numbered", "first"),
      newBlock("numbered", "second"),
      newBlock("numbered", "third"),
    ];
    expect(serialize(blocks)).toBe("1. first\n2. second\n3. third\n");
  });

  it("restarts numbering after an interruption", () => {
    const blocks = [
      newBlock("numbered", "a"),
      newBlock("numbered", "b"),
      newBlock("paragraph", "aside"),
      newBlock("numbered", "c"),
    ];
    expect(serialize(blocks)).toBe("1. a\n2. b\n\naside\n\n1. c\n");
  });

  it("fences code", () => {
    expect(serialize([newBlock("code", "let x = 1;")])).toBe("```\nlet x = 1;\n```\n");
  });

  it("ends with exactly one newline", () => {
    const out = serialize([newBlock("paragraph", "text")]);
    expect(out).toBe("text\n");
    expect(out.endsWith("\n\n")).toBe(false);
  });

  it("writes an empty document as almost nothing", () => {
    expect(serialize(emptyDocument()).trim()).toBe("");
  });
});

describe("round-tripping", () => {
  /**
   * The invariant the editor rests on: whatever is on screen survives a save and a reload.
   * `serialize(parse(x)) === x` is deliberately *not* claimed — `*` bullets become `-` — but
   * the block direction must be exact.
   */
  it("preserves blocks through a save and a load", () => {
    const original: Block[] = [
      newBlock("heading1", "Meeting notes"),
      newBlock("paragraph", "We covered three things."),
      newBlock("bullet", "pricing"),
      newBlock("bullet", "hiring"),
      { ...newBlock("todo", "send the summary"), checked: false },
      { ...newBlock("todo", "book the room"), checked: true },
      newBlock("numbered", "first"),
      newBlock("numbered", "second"),
      newBlock("quote", "we should ship Friday"),
      newBlock("code", "cargo test --workspace"),
      newBlock("divider"),
      newBlock("heading3", "Afterwards"),
      newBlock("paragraph", "Nothing else."),
    ];

    expect(shape(parse(serialize(original)))).toEqual(shape(original));
  });

  it("is stable across repeated saves", () => {
    const once = serialize(parse("# Title\n\n- a\n- b\n\n1. x\n2. y\n"));
    expect(serialize(parse(once))).toBe(once);
  });

  it("survives text containing marker-like characters", () => {
    const tricky: Block[] = [
      newBlock("paragraph", "3 - 2 = 1"),
      newBlock("paragraph", "a # b"),
      newBlock("paragraph", "> not a quote because it is mid-line"),
    ];
    // The third *will* re-read as a quote — a line starting with `>` is a quote by definition,
    // and that is Markdown's rule rather than a bug in the parser.
    const reread = shape(parse(serialize(tricky)));
    expect(reread[0]).toEqual({ type: "paragraph", text: "3 - 2 = 1" });
    expect(reread[1]).toEqual({ type: "paragraph", text: "a # b" });
  });

  it("preserves multi-line code exactly", () => {
    const code = newBlock("code", "line one\n  indented\n\nafter a blank");
    expect(shape(parse(serialize([code])))).toEqual([
      { type: "code", text: "line one\n  indented\n\nafter a blank" },
    ]);
  });
});

describe("shortcutFor", () => {
  it("recognises the markers as they are typed", () => {
    const cases: Array<[string, BlockType]> = [
      ["# ", "heading1"],
      ["## ", "heading2"],
      ["### ", "heading3"],
      ["- ", "bullet"],
      ["* ", "bullet"],
      ["1. ", "numbered"],
      ["3) ", "numbered"],
      ["> ", "quote"],
      ["[] ", "todo"],
      ["[ ] ", "todo"],
      ["```", "code"],
      ["---", "divider"],
    ];

    for (const [typed, type] of cases) {
      expect(shortcutFor(typed), typed).toEqual({ type, rest: "" });
    }
  });

  it("does not fire on ordinary text", () => {
    for (const text of ["hello", "#hashtag", "-dash", "1.5", "a - b", ""]) {
      expect(shortcutFor(text), text).toBeNull();
    }
  });
});

describe("blockAfter", () => {
  // What every editor does, and what makes a list usable.
  it("continues a list", () => {
    for (const type of ["bullet", "numbered", "todo"] as BlockType[]) {
      expect(blockAfter(newBlock(type, "something")).type).toBe(type);
    }
  });

  // Pressing Enter twice is how a list is escaped without reaching for the mouse.
  it("ends a list when the item is empty", () => {
    for (const type of ["bullet", "numbered", "todo"] as BlockType[]) {
      expect(blockAfter(newBlock(type, "")).type).toBe("paragraph");
      expect(blockAfter(newBlock(type, "   ")).type).toBe("paragraph");
    }
  });

  it("does not continue a heading or a quote", () => {
    expect(blockAfter(newBlock("heading1", "Title")).type).toBe("paragraph");
    expect(blockAfter(newBlock("quote", "said")).type).toBe("paragraph");
  });

  it("gives a new todo an unchecked state", () => {
    const next = blockAfter(newBlock("todo", "done thing"));
    expect(next.checked).toBe(false);
  });
});

describe("helpers", () => {
  it("knows which types are list items", () => {
    expect(isListItem("bullet")).toBe(true);
    expect(isListItem("numbered")).toBe(true);
    expect(isListItem("todo")).toBe(true);
    expect(isListItem("paragraph")).toBe(false);
    expect(isListItem("heading1")).toBe(false);
  });

  it("strips markers and empties for a plain-text view", () => {
    const blocks = [
      newBlock("heading1", "Title"),
      newBlock("divider"),
      newBlock("paragraph", ""),
      newBlock("bullet", "point"),
    ];
    expect(toPlainText(blocks)).toBe("Title\npoint");
  });
});

// ------------------------------------------------------------------ nesting

describe("nesting", () => {
  const bullets = (blocks: Block[]) =>
    blocks.map((b) => `${"  ".repeat(depthOf(b))}${b.text}`);

  it("reads indentation as depth", () => {
    const blocks = parse("- one\n  - two\n    - three\n- four\n");
    expect(blocks.map(depthOf)).toEqual([0, 1, 2, 0]);
  });

  // Four-space indentation is the other common convention. It reads as two levels, then the
  // skipped-level clamp brings it back to one — which is the shape its author meant.
  it("accepts four-space and tab indentation", () => {
    expect(parse("- one\n    - two\n").map(depthOf)).toEqual([0, 1]);
    expect(parse("- one\n\t- two\n").map(depthOf)).toEqual([0, 1]);
    // Two genuine levels, written four-space, survive as two.
    expect(parse("- a\n    - b\n        - c\n").map(depthOf)).toEqual([0, 1, 2]);
  });

  // Honouring a skipped level would render an item with no parent.
  it("clamps a level that was skipped", () => {
    expect(parse("- one\n      - deep\n").map(depthOf)).toEqual([0, 1]);
    expect(parse("      - orphan\n").map(depthOf)).toEqual([0]);
  });

  it("round-trips nested lists", () => {
    const text = "- one\n  - two\n    - three\n- four\n";
    expect(serialize(parse(text))).toBe(text);
  });

  it("only nests list items", () => {
    // A heading is never indented, so leading spaces there are not nesting.
    const blocks = parse("  # heading\n");
    expect(blocks[0].type).toBe("paragraph");
    expect(depthOf(blocks[0])).toBe(0);
  });

  it("numbers each level independently and restarts on re-entry", () => {
    const text = "1. one\n  1. a\n  2. b\n2. two\n  1. a\n";
    expect(serialize(parse(text))).toBe(text);
  });

  // A blank line between a parent and its nested child detaches them in most renderers.
  it("keeps a nested list attached to its parent", () => {
    const out = serialize(parse("1. parent\n  - child\n"));
    expect(out).toBe("1. parent\n  - child\n");
    expect(out).not.toContain("\n\n");
  });

  // Two different kinds at the same level are separate lists and should be separated.
  it("separates two sibling lists of different kinds", () => {
    expect(serialize(parse("- a\n1. b\n"))).toContain("\n\n");
  });

  it("indents an item under the one above it", () => {
    const blocks = parse("- one\n- two\n");
    expect(canIndent(blocks, 1)).toBe(true);
    expect(indent(blocks, 1).map(depthOf)).toEqual([0, 1]);
  });

  // The first item of a list has nothing to nest under.
  it("refuses to indent the first item", () => {
    const blocks = parse("- one\n- two\n");
    expect(canIndent(blocks, 0)).toBe(false);
    expect(indent(blocks, 0)).toBe(blocks);
  });

  it("refuses to indent past the maximum", () => {
    // A chain already at the limit: each line one level deeper than the last.
    const deep = Array.from({ length: MAX_DEPTH + 1 }, (_, i) => `${"  ".repeat(i)}- item${i}`);
    const blocks = parse(`${deep.join("\n")}\n`);

    expect(depthOf(blocks[blocks.length - 1])).toBe(MAX_DEPTH);
    expect(canIndent(blocks, blocks.length - 1)).toBe(false);
    expect(indent(blocks, blocks.length - 1)).toBe(blocks);
  });

  // Indenting is relative to the item directly above: you can only become its child or its
  // sibling's child, never skip a level. So an item already nested under its parent cannot go
  // deeper until something sits beside it at that depth.
  it("refuses to indent an item already nested under its parent", () => {
    const blocks = parse("- one\n  - a\n");
    expect(canIndent(blocks, 1)).toBe(false);
  });

  it("refuses to outdent at the top level", () => {
    const blocks = parse("- one\n");
    expect(canOutdent(blocks, 0)).toBe(false);
    expect(outdent(blocks, 0)).toBe(blocks);
  });

  // Moving a parent alone would flatten the structure the user built.
  it("carries nested children when indenting", () => {
    const blocks = parse("- one\n- two\n  - child\n    - grandchild\n- three\n");
    const after = indent(blocks, 1);
    expect(after.map(depthOf)).toEqual([0, 1, 2, 3, 0]);
    expect(bullets(after)[4]).toBe("three");
  });

  it("carries nested children when outdenting", () => {
    const blocks = parse("- one\n  - two\n    - child\n- three\n");
    const after = outdent(blocks, 1);
    expect(after.map(depthOf)).toEqual([0, 0, 1, 0]);
  });

  // A sibling at the same level is not a child and must not move.
  it("leaves siblings alone", () => {
    const blocks = parse("- one\n  - a\n  - b\n  - c\n");
    // `b` nests under `a`; `c` is `b`'s sibling, not its child, so it stays put.
    expect(indent(blocks, 2).map(depthOf)).toEqual([0, 1, 2, 1]);
  });

  it("a block with no depth is top level", () => {
    expect(depthOf(newBlock("bullet"))).toBe(0);
    expect(depthOf({ id: "x", type: "paragraph", text: "", depth: 3 })).toBe(0);
  });
});

// ------------------------------------------------------------ tables and images

describe("tables", () => {
  const md = "| a | b |\n| --- | --- |\n| 1 | 2 |";

  it("parses a table as one block", () => {
    const blocks = parse(`${md}\n`);
    expect(blocks).toHaveLength(1);
    expect(blocks[0].type).toBe("table");
  });

  it("round-trips", () => {
    expect(serialize(parse(`${md}\n`))).toBe(`${md}\n`);
  });

  it("reads cells as a grid, without the alignment row", () => {
    expect(tableRows(parse(`${md}\n`)[0])).toEqual([
      ["a", "b"],
      ["1", "2"],
    ]);
  });

  // A ragged table would otherwise make the editor reason about missing cells.
  it("squares off a ragged table", () => {
    const rows = tableRows(parse("| a | b | c |\n| --- | --- | --- |\n| 1 |\n")[0]);
    expect(rows).toEqual([
      ["a", "b", "c"],
      ["1", "", ""],
    ]);
  });

  it("survives a pipe inside a cell", () => {
    const block = { id: "t", type: "table" as const, text: tableText([["a|b"], ["c"]]) };
    expect(tableRows(block)).toEqual([["a|b"], ["c"]]);
  });

  it("a new table has a header and one row", () => {
    expect(tableRows(newTable())).toEqual([
      ["", ""],
      ["", ""],
    ]);
  });

  // The alignment row is syntax, not content — treating it as a row would show `---` as data.
  it("never returns the alignment row as data", () => {
    for (const row of tableRows(parse(`${md}\n`)[0])) {
      expect(row.join("")).not.toContain("---");
    }
  });

  // A pipe line with no alignment row under it is not a table.
  it("leaves a lone pipe line as a paragraph", () => {
    expect(parse("| not a table |\n")[0].type).toBe("paragraph");
  });
});

describe("images", () => {
  it("parses a line that is only an image", () => {
    const blocks = parse("![a cat](/tmp/cat.png)\n");
    expect(blocks[0].type).toBe("image");
    expect(imageParts(blocks[0])).toEqual({ alt: "a cat", src: "/tmp/cat.png" });
  });

  it("round-trips", () => {
    const text = "![a cat](/tmp/cat.png)\n";
    expect(serialize(parse(text))).toBe(text);
  });

  // Turning this into a block would drop the surrounding words.
  it("leaves an image with text around it as a paragraph", () => {
    expect(parse("see ![a](b) here\n")[0].type).toBe("paragraph");
  });

  it("handles an empty source and empty alt", () => {
    expect(imageParts(newImage())).toEqual({ alt: "", src: "" });
    expect(serialize([newImage("/x.png")])).toBe("![](/x.png)\n");
  });

  // Brackets in alt text would terminate the `![...]` early.
  it("strips brackets from alt text", () => {
    expect(imageText("a [b] c", "/x.png")).toBe("![a b c](/x.png)");
  });
});
