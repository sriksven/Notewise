/**
 * Formatting *within* a line.
 *
 * `blocks.ts` handles the shape of a document — what is a heading, what is a list item. This
 * handles what happens inside one: bold, italic, code, strikethrough and links.
 *
 * # Markdown stays the source of truth
 *
 * The stored text is still `**bold**`, for every reason given in `blocks.ts`: the body is
 * embedded, indexed, read by the agent, and written to a vault as a file someone opens
 * elsewhere. What changes here is that the editor *renders* it — a block that does not have
 * the caret shows formatted text, and clicking into it reveals the markers again.
 *
 * That is the trade a textarea-per-block makes possible. A `contenteditable` would show
 * formatted text while editing, at the cost of owning selection, undo, paste and IME
 * behaviour. Showing the markers only on the line being edited costs one click of awkwardness
 * and keeps all of that native.
 */

export type Mark = "bold" | "italic" | "code" | "strike";

export interface Span {
  text: string;
  marks: Mark[];
  /** Set when this span is a link. */
  href?: string;
}

/** The Markdown delimiters for each mark, and the order they are tried in. */
const DELIMITERS: Array<{ mark: Mark; open: string; close: string }> = [
  // Longest first: `**` must win over `*`, or bold parses as two empty italics.
  { mark: "bold", open: "**", close: "**" },
  { mark: "strike", open: "~~", close: "~~" },
  { mark: "code", open: "`", close: "`" },
  { mark: "italic", open: "*", close: "*" },
  { mark: "italic", open: "_", close: "_" },
];

const LINK = /^\[([^\]]*)\]\(([^)\s]+)\)/;

/**
 * Split a line into formatted spans.
 *
 * A hand-rolled scanner rather than a Markdown library, for the same reason `blocks.ts` is:
 * this has to round-trip exactly with what the editor writes, and a full implementation would
 * interpret constructs the editor cannot produce and cannot show.
 *
 * Code wins over everything inside it — `` `**not bold**` `` is four literal characters and a
 * word — which is why it is scanned as an opaque run rather than recursed into.
 */
export function parseInline(text: string): Span[] {
  const spans: Span[] = [];
  let plain = "";
  let index = 0;

  const flush = () => {
    if (plain) spans.push({ text: plain, marks: [] });
    plain = "";
  };

  while (index < text.length) {
    const rest = text.slice(index);

    const link = rest.match(LINK);
    if (link) {
      flush();
      spans.push({ text: link[1] || link[2], marks: [], href: link[2] });
      index += link[0].length;
      continue;
    }

    const delimiter = DELIMITERS.find(
      (candidate) => rest.startsWith(candidate.open) && rest.length > candidate.open.length,
    );

    if (delimiter) {
      const closeAt = text.indexOf(delimiter.close, index + delimiter.open.length);
      const inner =
        closeAt === -1 ? null : text.slice(index + delimiter.open.length, closeAt);

      // An unclosed delimiter is literal text, not emphasis that never ends. So is an empty
      // one: `**` on its own is two asterisks.
      if (inner) {
        flush();
        if (delimiter.mark === "code") {
          // Opaque: nothing inside a code span is formatting.
          spans.push({ text: inner, marks: ["code"] });
        } else {
          // Nested marks are kept, so `**bold *and italic* **` works.
          for (const span of parseInline(inner)) {
            spans.push({ ...span, marks: [delimiter.mark, ...span.marks] });
          }
        }
        index = closeAt + delimiter.close.length;
        continue;
      }
    }

    plain += text[index];
    index += 1;
  }

  flush();
  return spans;
}

/** Whether a line contains anything this renders differently from plain text. */
export function hasFormatting(text: string): boolean {
  const spans = parseInline(text);
  return spans.some((span) => span.marks.length > 0 || span.href !== undefined);
}

export interface Selection {
  text: string;
  start: number;
  end: number;
}

const MARKERS: Record<Mark, string> = {
  bold: "**",
  italic: "*",
  code: "`",
  strike: "~~",
};

/**
 * Apply or remove a mark over a selection.
 *
 * Returns the new text and where the selection should sit afterwards — the caller puts it
 * back, because a textarea that loses the user's selection on every ⌘B is unusable.
 *
 * Toggles rather than only wrapping. Pressing ⌘B on text that is already bold should unbold
 * it, and an editor where the same key only ever adds markers accumulates `****bold****`.
 *
 * With an empty selection it inserts the pair and puts the caret between them, so ⌘B then
 * typing produces bold text — which is what the shortcut means when nothing is selected.
 */
export function toggleMark(selection: Selection, mark: Mark): Selection {
  const marker = MARKERS[mark];
  const { text, start, end } = selection;
  const selected = text.slice(start, end);

  // Already wrapped, inside the selection: `**bold**` selected whole.
  if (
    selected.length >= marker.length * 2 &&
    selected.startsWith(marker) &&
    selected.endsWith(marker)
  ) {
    const inner = selected.slice(marker.length, selected.length - marker.length);
    return {
      text: text.slice(0, start) + inner + text.slice(end),
      start,
      end: start + inner.length,
    };
  }

  // Already wrapped, just outside the selection: `**bold**` with only `bold` selected.
  //
  // The run lengths must match *exactly*, which is not the same as "the adjacent characters
  // are the marker". Toggling italic inside `**bold**` sees a `*` on each side and would
  // otherwise strip one from each, turning bold into italic and destroying what was there.
  // Refusing to unwrap in an ambiguous run is safe; corrupting existing formatting is not.
  const symbol = marker[0];
  if (
    runLength(text, start - 1, -1, symbol) === marker.length &&
    runLength(text, end, 1, symbol) === marker.length
  ) {
    return {
      text: text.slice(0, start - marker.length) + selected + text.slice(end + marker.length),
      start: start - marker.length,
      end: end - marker.length,
    };
  }

  return {
    text: text.slice(0, start) + marker + selected + marker + text.slice(end),
    start: start + marker.length,
    end: end + marker.length,
  };
}

/** How many `symbol` characters run from `from`, walking in `step`. */
function runLength(text: string, from: number, step: 1 | -1, symbol: string): number {
  let count = 0;
  let index = from;
  while (index >= 0 && index < text.length && text[index] === symbol) {
    count += 1;
    index += step;
  }
  return count;
}

/**
 * The keyboard chord for a mark, or `null`.
 *
 * ⌘K is deliberately absent: it is the app's search shortcut, and a link chord that only
 * works when the caret happens to be in a note would be worse than a documented `[text](url)`.
 */
export function markForKey(key: string, shift: boolean): Mark | null {
  switch (key.toLowerCase()) {
    case "b":
      return shift ? null : "bold";
    case "i":
      return shift ? null : "italic";
    case "e":
      return shift ? null : "code";
    case "x":
      return shift ? "strike" : null;
    default:
      return null;
  }
}
