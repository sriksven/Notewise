/**
 * Just enough Markdown for what a summarizer writes.
 *
 * Models emit `**Decisions:**` and `* bullet` whether or not they were asked to, and rendering
 * that as plain text puts literal asterisks in front of the user — the one screen meant to read
 * like a finished document instead looks like a bug.
 *
 * Deliberately not a Markdown library. This handles headings, bullets, numbered lists and bold,
 * which is the whole of what appears in practice; links, tables, images and raw HTML are not
 * supported and are left as text. Nothing here interprets HTML, so there is no injection path —
 * the output is spans of text with a flag, rendered by React as text.
 */

export interface Span {
  text: string;
  bold: boolean;
}

export type Block =
  | { kind: "heading"; spans: Span[] }
  | { kind: "paragraph"; spans: Span[] }
  | { kind: "list"; items: Span[][] };

/** A line that is nothing but bold text, which models use as a heading. */
const BOLD_ONLY = /^\*\*(.+?)\*\*:?\s*$/;
const HEADING = /^#{1,6}\s+(.*)$/;
const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;

/** Split on `**bold**`, keeping the runs between. */
export function parseInline(text: string): Span[] {
  const spans: Span[] = [];
  let rest = text;

  while (rest.length > 0) {
    const open = rest.indexOf("**");
    if (open === -1) break;

    const close = rest.indexOf("**", open + 2);
    // An unclosed `**` is literal text, not the start of emphasis that never ends.
    if (close === -1) break;

    if (open > 0) spans.push({ text: rest.slice(0, open), bold: false });
    const inner = rest.slice(open + 2, close);
    if (inner) spans.push({ text: inner, bold: true });
    rest = rest.slice(close + 2);
  }

  if (rest.length > 0) spans.push({ text: rest, bold: false });
  return spans.length > 0 ? spans : [{ text, bold: false }];
}

export function parseMarkdown(source: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let items: Span[][] = [];

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    blocks.push({ kind: "paragraph", spans: parseInline(paragraph.join(" ")) });
    paragraph = [];
  };

  const flushList = () => {
    if (items.length === 0) return;
    blocks.push({ kind: "list", items });
    items = [];
  };

  for (const line of source.split("\n")) {
    const trimmed = line.trim();

    if (trimmed === "") {
      flushParagraph();
      flushList();
      continue;
    }

    const heading = HEADING.exec(trimmed) ?? BOLD_ONLY.exec(trimmed);
    if (heading) {
      flushParagraph();
      flushList();
      // The trailing colon goes: models write `**Decisions:**` and `**Decisions**:` for the
      // same thing, and one heading with a colon beside one without reads as a mistake.
      blocks.push({ kind: "heading", spans: parseInline(heading[1].replace(/:\s*$/, "")) });
      continue;
    }

    const item = BULLET.exec(line) ?? NUMBERED.exec(line);
    if (item) {
      flushParagraph();
      items.push(parseInline(item[1]));
      continue;
    }

    flushList();
    paragraph.push(trimmed);
  }

  flushParagraph();
  flushList();
  return blocks;
}
