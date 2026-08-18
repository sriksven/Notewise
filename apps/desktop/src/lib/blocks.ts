/**
 * The note document model.
 *
 * # Blocks in memory, Markdown on disk
 *
 * The editor thinks in blocks; storage keeps Markdown. That is deliberate, and the reason is
 * not aesthetics.
 *
 * A note's body is opaque to the engine — but it is not unused. It is embedded for semantic
 * search, indexed for full-text search, read by the agent, quoted in grounded answers, and
 * written out by the vault connector as a file someone opens in Obsidian. Storing
 * `[{"type":"paragraph","text":"..."}]` would put JSON syntax into every one of those: the
 * embedding would encode punctuation, the full-text index would match on the word "paragraph",
 * and the vault would fill with files nothing can read.
 *
 * Markdown costs one parse on load and one serialize on save, and keeps all of it working.
 * It also means every note written before this editor existed opens correctly, with no
 * migration — plain text is already valid Markdown.
 *
 * # What round-tripping guarantees
 *
 * `parse(serialize(blocks))` returns the same blocks. `serialize(parse(text))` does *not*
 * always return the same text — `*` and `-` bullets both become `-`, and trailing whitespace
 * is dropped. That asymmetry is the right way round: the editor's state survives exactly, and
 * only cosmetic variation in hand-written Markdown is normalised.
 */

export type BlockType =
  | "paragraph"
  | "heading1"
  | "heading2"
  | "heading3"
  | "bullet"
  | "numbered"
  | "todo"
  | "quote"
  | "code"
  | "divider"
  | "table"
  | "image";

export interface Block {
  /** Stable across edits, so React keys do not reorder and steal focus. */
  id: string;
  type: BlockType;
  text: string;
  /** Only meaningful for `todo`. */
  checked?: boolean;
  /**
   * Nesting level for list items, `0` at the top. Ignored for every other type.
   *
   * Optional so that a block built before nesting existed — or by a test — is still a valid
   * top-level block. {@link depthOf} is the only thing that should read it.
   */
  depth?: number;
}

/** How deep a list may nest. */
export const MAX_DEPTH = 5;

/** Spaces per level, on disk. Two is what every Markdown renderer accepts for `-` lists. */
const INDENT = "  ";

/** A block's nesting level, defaulting to top level and never out of range. */
export function depthOf(block: Block): number {
  if (!isListItem(block.type)) return 0;
  return Math.min(Math.max(block.depth ?? 0, 0), MAX_DEPTH);
}

let counter = 0;

/**
 * A key for a block.
 *
 * Not persisted — ids exist only to keep React from re-mounting a row when its neighbour
 * changes, which would move the caret. A counter rather than `crypto.randomUUID()` because it
 * is cheaper and nothing outside this module ever sees the value.
 */
export function blockId(): string {
  counter += 1;
  return `b${counter}`;
}

export function newBlock(type: BlockType = "paragraph", text = ""): Block {
  return { id: blockId(), type, text, ...(type === "todo" ? { checked: false } : {}) };
}

/** A document that has never been written to still needs one block to type into. */
export function emptyDocument(): Block[] {
  return [newBlock()];
}

/** Whether a type renders as a single line that cannot contain newlines. */
export function isMultiline(type: BlockType): boolean {
  return type === "code";
}

/** Types the editor draws itself rather than showing as an editable line of text. */
export function isStructural(type: BlockType): boolean {
  return type === "divider" || type === "table" || type === "image";
}

// ---------------------------------------------------------------- tables

/**
 * A table's cells, as a grid.
 *
 * Stored in the block's `text` as the Markdown table itself, so the body on disk stays a
 * document anything can read — the same reason the whole editor keeps Markdown rather than
 * JSON. This parses that text into a grid when the editor needs one and writes it back after.
 *
 * The first row is the header. Markdown tables have no way to express a table without one.
 */
export function tableRows(block: Block): string[][] {
  const rows = block.text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.startsWith("|") && !ALIGNMENT_ROW.test(line))
    .map((line) =>
      line
        // Only *unescaped* pipes delimit a cell. Splitting on every `|` first would cut a cell
        // containing `\|` in two and leave a stray backslash behind — which is what happened.
        .replace(/^\|/, "")
        .replace(/(?<!\\)\|$/, "")
        .split(/(?<!\\)\|/)
        .map((cell) => cell.trim().replace(/\\\|/g, "|")),
    );

  if (rows.length === 0) return [["", ""], ["", ""]];

  // Ragged input is normalised to the widest row, so the grid is always rectangular and the
  // editor never has to reason about a missing cell.
  const width = Math.max(...rows.map((r) => r.length));
  return rows.map((row) => [...row, ...Array(width - row.length).fill("")]);
}

/** Write a grid back to Markdown table syntax. */
export function tableText(rows: string[][]): string {
  const width = Math.max(1, ...rows.map((r) => r.length));
  const line = (cells: string[]) =>
    `| ${Array.from({ length: width }, (_, i) => (cells[i] ?? "").replace(/\|/g, "\\|")).join(" | ")} |`;

  const [header = [], ...body] = rows;
  return [line(header), `|${" --- |".repeat(width)}`, ...body.map(line)].join("\n");
}

/** A fresh table: a header row and one empty row under it. */
export function newTable(columns = 2, bodyRows = 1): Block {
  const rows = Array.from({ length: bodyRows + 1 }, () => Array(columns).fill(""));
  return { id: blockId(), type: "table", text: tableText(rows) };
}

/** The `| --- | --- |` line under a table's header. Never a row of data. */
const ALIGNMENT_ROW = /^\|(\s*:?-{3,}:?\s*\|)+$/;

// ---------------------------------------------------------------- images

/** An image block's source and alt text, read from its `![alt](src)` body. */
export function imageParts(block: Block): { alt: string; src: string } {
  const match = block.text.match(IMAGE_LINE);
  return { alt: match?.[1] ?? "", src: match?.[2] ?? "" };
}

export function imageText(alt: string, src: string): string {
  return `![${alt.replace(/[[\]]/g, "")}](${src.trim()})`;
}

export function newImage(src = "", alt = ""): Block {
  return { id: blockId(), type: "image", text: imageText(alt, src) };
}

const IMAGE_LINE = /^!\[([^\]]*)\]\(([^)]*)\)$/;

// ---------------------------------------------------------------- parsing

interface Rule {
  type: BlockType;
  /** Matches the marker and captures the remaining text. */
  pattern: RegExp;
}

/**
 * Line prefixes, longest-first.
 *
 * Order matters: `###` has to be tried before `##`, and the todo forms before the plain
 * bullet, or `- [ ] thing` parses as a bullet whose text is `[ ] thing`.
 */
const RULES: Rule[] = [
  { type: "heading3", pattern: /^###\s+(.*)$/ },
  { type: "heading2", pattern: /^##\s+(.*)$/ },
  { type: "heading1", pattern: /^#\s+(.*)$/ },
  // List markers may be indented; headings and quotes may not, because a leading space there
  // is not meaningful and treating it as nesting would indent a heading nobody indented.
  { type: "todo", pattern: /^[ \t]*[-*]\s+\[[xX]\]\s*(.*)$/ },
  { type: "todo", pattern: /^[ \t]*[-*]\s+\[\s?\]\s*(.*)$/ },
  { type: "bullet", pattern: /^[ \t]*[-*]\s+(.*)$/ },
  { type: "numbered", pattern: /^[ \t]*\d+[.)]\s+(.*)$/ },
  { type: "quote", pattern: /^>\s?(.*)$/ },
];

const CHECKED = /^[ \t]*[-*]\s+\[[xX]\]/;

/**
 * Nesting level implied by a line's leading whitespace.
 *
 * Two spaces per level, with a tab counting as one level. Four-space indentation — the other
 * common convention, and what many editors insert — reads as two levels here, and is then
 * brought back to one by the skipped-level clamp in {@link parse}, since nothing precedes it at
 * that depth. The pair works out: either convention opens with the shape its author intended.
 */
function indentOf(line: string): number {
  const lead = line.match(/^[ \t]*/)?.[0] ?? "";
  const spaces = lead.replace(/\t/g, INDENT).length;
  return Math.min(Math.floor(spaces / INDENT.length), MAX_DEPTH);
}

/** A horizontal rule: three or more `-`, `*` or `_`, optionally spaced. */
const DIVIDER = /^\s*([-*_])(\s*\1){2,}\s*$/;

/**
 * Read Markdown into blocks.
 *
 * Deliberately a *line* parser, not a Markdown implementation. It understands the block
 * structure the editor can produce and leaves everything inside a line — bold, links, inline
 * code — as literal text, which is what the editor shows and what round-trips exactly. A full
 * Markdown parser here would let the editor mangle documents it does not know how to render.
 */
export function parse(markdown: string): Block[] {
  if (!markdown.trim()) return emptyDocument();

  const lines = markdown.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];

  let index = 0;
  while (index < lines.length) {
    const line = lines[index];

    // Fenced code, taken whole. Its contents are not parsed — a `# comment` inside a shell
    // snippet is a comment, not a heading.
    if (/^```/.test(line.trim())) {
      const body: string[] = [];
      index += 1;
      while (index < lines.length && !/^```/.test(lines[index].trim())) {
        body.push(lines[index]);
        index += 1;
      }
      index += 1; // the closing fence, or the end of input
      blocks.push({ id: blockId(), type: "code", text: body.join("\n") });
      continue;
    }

    // A table, taken whole: a pipe row followed by an alignment row. Checked before `index`
    // advances because the run spans several lines.
    if (line.trim().startsWith("|") && ALIGNMENT_ROW.test((lines[index + 1] ?? "").trim())) {
      const body: string[] = [];
      while (index < lines.length && lines[index].trim().startsWith("|")) {
        body.push(lines[index].trim());
        index += 1;
      }
      blocks.push({ id: blockId(), type: "table", text: body.join("\n") });
      continue;
    }

    index += 1;

    // An image is a line that is nothing but an image. `![a](b)` with text around it stays a
    // paragraph, because turning it into a block would drop that text.
    if (IMAGE_LINE.test(line.trim())) {
      blocks.push({ id: blockId(), type: "image", text: line.trim() });
      continue;
    }

    if (DIVIDER.test(line)) {
      blocks.push({ id: blockId(), type: "divider", text: "" });
      continue;
    }

    // Blank lines separate blocks in Markdown; they are not blocks themselves.
    if (!line.trim()) continue;

    const rule = RULES.find((candidate) => candidate.pattern.test(line));
    if (rule) {
      const text = line.match(rule.pattern)?.[1] ?? "";

      // A level may only be entered one at a time. A file indenting straight from top level to
      // four is describing a shape no editor can produce, and honouring it would render a list
      // item with no parent — so it is clamped to one deeper than what precedes it.
      const previous = blocks[blocks.length - 1];
      const ceiling = previous && isListItem(previous.type) ? depthOf(previous) + 1 : 0;
      const depth = isListItem(rule.type) ? Math.min(indentOf(line), ceiling) : 0;

      // Only carried when it is not zero. Stamping `depth: 0` onto every block would change
      // the shape of every block the editor has ever produced, for no information — `depthOf`
      // already reads a missing depth as top level.
      const nesting = depth > 0 ? { depth } : {};

      blocks.push(
        rule.type === "todo"
          ? { id: blockId(), type: "todo", text, checked: CHECKED.test(line), ...nesting }
          : { id: blockId(), type: rule.type, text, ...nesting },
      );
      continue;
    }

    blocks.push({ id: blockId(), type: "paragraph", text: line });
  }

  return blocks.length > 0 ? blocks : emptyDocument();
}

// ---------------------------------------------------------------- serializing

function marker(block: Block): string {
  const pad = INDENT.repeat(depthOf(block));
  switch (block.type) {
    case "heading1":
      return `# ${block.text}`;
    case "heading2":
      return `## ${block.text}`;
    case "heading3":
      return `### ${block.text}`;
    case "bullet":
      return `${pad}- ${block.text}`;
    case "todo":
      return `${pad}- [${block.checked ? "x" : " "}] ${block.text}`;
    case "quote":
      return `> ${block.text}`;
    case "code":
      return `\`\`\`\n${block.text}\n\`\`\``;
    case "divider":
      return "---";
    // Both keep their Markdown in `text`, so there is nothing to rebuild here.
    case "table":
    case "image":
      return block.text;
    default:
      return block.text;
  }
}

/**
 * Write blocks back to Markdown.
 *
 * Numbered lists are renumbered from their position in the run rather than preserving what was
 * typed: a list a user has reordered should read 1, 2, 3, and Markdown renderers renumber
 * anyway, so keeping the original digits would make the file disagree with every view of it.
 *
 * Consecutive list items are not separated by blank lines — that would break the list in most
 * renderers. Everything else is.
 */
export function serialize(blocks: Block[]): string {
  const lines: string[] = [];
  // One counter per level. Going deeper starts a fresh count; coming back up discards it, so a
  // sub-list between `1.` and `2.` does not make the outer list read 1, 2, 3.
  let ordinals: number[] = [];

  blocks.forEach((block, index) => {
    const depth = depthOf(block);

    if (block.type === "numbered") {
      ordinals = ordinals.slice(0, depth + 1);
      ordinals[depth] = (ordinals[depth] ?? 0) + 1;
      lines.push(`${INDENT.repeat(depth)}${ordinals[depth]}. ${block.text}`);
    } else {
      // Any other block ends every run. A bullet nested under a numbered item does not
      // continue its numbering, and a paragraph certainly does not.
      ordinals = isListItem(block.type) ? ordinals.slice(0, depth) : [];
      lines.push(marker(block));
    }

    const next = blocks[index + 1];
    if (!next) return;

    // Blank lines break a list in most renderers. Consecutive items stay together when they are
    // the same kind, *or* when the level changes — a bullet nested under a numbered item is a
    // continuation of that list, and a blank line there would detach it from its parent. Two
    // different kinds at the *same* level are genuinely separate lists and do get a break.
    const continuesList =
      isListItem(block.type) &&
      isListItem(next.type) &&
      (block.type === next.type || depthOf(next) !== depthOf(block));
    if (!continuesList) lines.push("");
  });

  // A single trailing newline, and no leading or trailing blank lines.
  return `${lines.join("\n").replace(/\n+$/, "")}\n`;
}

/**
 * Whether the block at `index` may be nested one level deeper.
 *
 * Only a list item can nest, and only under a list item that is already at least as deep. The
 * first item of a list has nothing to nest under, so it cannot be indented — which is what
 * stops a document from starting at depth 3 with no parent anywhere.
 */
export function canIndent(blocks: Block[], index: number): boolean {
  const block = blocks[index];
  if (!block || !isListItem(block.type)) return false;
  if (depthOf(block) >= MAX_DEPTH) return false;

  const previous = blocks[index - 1];
  return !!previous && isListItem(previous.type) && depthOf(previous) >= depthOf(block);
}

export function canOutdent(blocks: Block[], index: number): boolean {
  const block = blocks[index];
  return !!block && isListItem(block.type) && depthOf(block) > 0;
}

/**
 * Indent a list item and everything nested beneath it.
 *
 * Children move with the parent. Indenting a parent alone would leave its children at the same
 * level as it, which silently flattens the structure the user built.
 */
export function indent(blocks: Block[], index: number): Block[] {
  if (!canIndent(blocks, index)) return blocks;
  return shift(blocks, index, +1);
}

/** Outdent a list item and everything nested beneath it. */
export function outdent(blocks: Block[], index: number): Block[] {
  if (!canOutdent(blocks, index)) return blocks;
  return shift(blocks, index, -1);
}

/** Move a block and its descendants by one level, leaving everything else alone. */
function shift(blocks: Block[], index: number, by: 1 | -1): Block[] {
  const base = depthOf(blocks[index]);
  const next = blocks.slice();

  for (let i = index; i < next.length; i += 1) {
    const block = next[i];
    // The subtree ends at the first item that is not deeper than the one being moved.
    if (i > index && (!isListItem(block.type) || depthOf(block) <= base)) break;

    const moved = Math.min(Math.max(depthOf(block) + by, 0), MAX_DEPTH);
    next[i] = { ...block, depth: moved };
  }

  return next;
}

export function isListItem(type: BlockType): boolean {
  return type === "bullet" || type === "numbered" || type === "todo";
}

// ---------------------------------------------------------------- editing

/**
 * The Markdown shortcut a line's opening characters imply, if any.
 *
 * Applied as the user types, so `# ` at the start of an empty paragraph turns it into a
 * heading and disappears. Only fires on a paragraph — retyping `- ` inside a bullet should
 * insert a literal dash, not nest anything.
 */
export function shortcutFor(text: string): { type: BlockType; rest: string } | null {
  const shortcuts: Array<[RegExp, BlockType]> = [
    [/^###\s$/, "heading3"],
    [/^##\s$/, "heading2"],
    [/^#\s$/, "heading1"],
    [/^\[\]\s$/, "todo"],
    [/^\[\s\]\s$/, "todo"],
    [/^[-*]\s$/, "bullet"],
    [/^\d+[.)]\s$/, "numbered"],
    [/^>\s$/, "quote"],
    [/^```$/, "code"],
    [/^---$/, "divider"],
  ];

  for (const [pattern, type] of shortcuts) {
    if (pattern.test(text)) return { type, rest: "" };
  }
  return null;
}

/**
 * What pressing Enter at the end of this block should produce.
 *
 * Continuing a list is the behaviour people expect from every editor. Headings and quotes do
 * not continue — one heading is rarely followed by another — and an empty list item ends the
 * list instead of adding a third empty one, which is how a list is escaped without reaching
 * for the mouse.
 */
export function blockAfter(block: Block): Block {
  if (isListItem(block.type) && block.text.trim() !== "") {
    return newBlock(block.type);
  }
  return newBlock("paragraph");
}

/** Plain text, for a preview or a character count. Markers are not content. */
export function toPlainText(blocks: Block[]): string {
  return blocks
    .filter((block) => block.type !== "divider")
    .map((block) => block.text)
    .filter((text) => text.trim() !== "")
    .join("\n");
}
