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
  | "divider";

export interface Block {
  /** Stable across edits, so React keys do not reorder and steal focus. */
  id: string;
  type: BlockType;
  text: string;
  /** Only meaningful for `todo`. */
  checked?: boolean;
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
  { type: "todo", pattern: /^[-*]\s+\[[xX]\]\s*(.*)$/ },
  { type: "todo", pattern: /^[-*]\s+\[\s?\]\s*(.*)$/ },
  { type: "bullet", pattern: /^[-*]\s+(.*)$/ },
  { type: "numbered", pattern: /^\d+[.)]\s+(.*)$/ },
  { type: "quote", pattern: /^>\s?(.*)$/ },
];

const CHECKED = /^[-*]\s+\[[xX]\]/;

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

    index += 1;

    if (DIVIDER.test(line)) {
      blocks.push({ id: blockId(), type: "divider", text: "" });
      continue;
    }

    // Blank lines separate blocks in Markdown; they are not blocks themselves.
    if (!line.trim()) continue;

    const rule = RULES.find((candidate) => candidate.pattern.test(line));
    if (rule) {
      const text = line.match(rule.pattern)?.[1] ?? "";
      blocks.push(
        rule.type === "todo"
          ? { id: blockId(), type: "todo", text, checked: CHECKED.test(line) }
          : { id: blockId(), type: rule.type, text },
      );
      continue;
    }

    blocks.push({ id: blockId(), type: "paragraph", text: line });
  }

  return blocks.length > 0 ? blocks : emptyDocument();
}

// ---------------------------------------------------------------- serializing

function marker(block: Block): string {
  switch (block.type) {
    case "heading1":
      return `# ${block.text}`;
    case "heading2":
      return `## ${block.text}`;
    case "heading3":
      return `### ${block.text}`;
    case "bullet":
      return `- ${block.text}`;
    case "todo":
      return `- [${block.checked ? "x" : " "}] ${block.text}`;
    case "quote":
      return `> ${block.text}`;
    case "code":
      return `\`\`\`\n${block.text}\n\`\`\``;
    case "divider":
      return "---";
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
  let ordinal = 0;

  blocks.forEach((block, index) => {
    if (block.type === "numbered") {
      ordinal += 1;
      lines.push(`${ordinal}. ${block.text}`);
    } else {
      ordinal = 0;
      lines.push(marker(block));
    }

    const next = blocks[index + 1];
    if (!next) return;

    const bothListItems =
      isListItem(block.type) && isListItem(next.type) && block.type === next.type;
    if (!bothListItems) lines.push("");
  });

  // A single trailing newline, and no leading or trailing blank lines.
  return `${lines.join("\n").replace(/\n+$/, "")}\n`;
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
