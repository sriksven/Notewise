import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  Check,
  Code,
  Heading1,
  Heading2,
  Heading3,
  List,
  ListOrdered,
  Minus,
  Quote,
  SquareCheck,
  Type,
} from "lucide-react";

import {
  blockAfter,
  isListItem,
  isMultiline,
  newBlock,
  shortcutFor,
  type Block,
  type BlockType,
} from "../lib/blocks";

interface Props {
  blocks: Block[];
  onChange: (blocks: Block[]) => void;
  placeholder?: string;
  /** Focus the first block on mount — for a note the user just created. */
  autoFocus?: boolean;
}

/** What the slash menu offers, in the order it offers it. */
const MENU: Array<{ type: BlockType; label: string; hint: string; Icon: typeof Type }> = [
  { type: "paragraph", label: "Text", hint: "", Icon: Type },
  { type: "heading1", label: "Heading 1", hint: "#", Icon: Heading1 },
  { type: "heading2", label: "Heading 2", hint: "##", Icon: Heading2 },
  { type: "heading3", label: "Heading 3", hint: "###", Icon: Heading3 },
  { type: "bullet", label: "Bulleted list", hint: "-", Icon: List },
  { type: "numbered", label: "Numbered list", hint: "1.", Icon: ListOrdered },
  { type: "todo", label: "To-do", hint: "[]", Icon: SquareCheck },
  { type: "quote", label: "Quote", hint: ">", Icon: Quote },
  { type: "code", label: "Code", hint: "```", Icon: Code },
  { type: "divider", label: "Divider", hint: "---", Icon: Minus },
];

/**
 * A block editor.
 *
 * # Why a textarea per block rather than one contenteditable
 *
 * `contenteditable` is how most rich editors are built and it is a large amount of work to
 * make behave: browsers disagree about what Enter inserts, paste arrives as arbitrary HTML,
 * undo is the browser's rather than the document's, and the DOM can be edited into states the
 * model cannot represent. Getting that right is what ProseMirror and Lexical are for, and
 * taking one of those is a dependency and a serialization format to own.
 *
 * A textarea per block gives up inline formatting toolbars — bold is typed as `**bold**` — and
 * gets, in exchange, native text editing that already works: selection, undo, spellcheck, IME
 * composition, accessibility, and paste as plain text. For a notes pane during a meeting, the
 * thing that matters is that typing never does anything surprising.
 *
 * # Focus
 *
 * The one thing this has to get right. Splitting, merging and converting blocks all re-render
 * the list, and if focus is not restored deliberately the caret jumps to the top of the
 * document mid-sentence. Every operation that changes the structure records where the caret
 * should land, and a layout effect puts it there before the browser paints.
 */
export function BlockEditor({ blocks, onChange, placeholder, autoFocus }: Props) {
  /** Where to put the caret after the next render, if anywhere. */
  const pending = useRef<{ id: string; offset: number | "end" } | null>(null);
  const fields = useRef(new Map<string, HTMLTextAreaElement>());
  /** Which block has the slash menu open. */
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const [menuIndex, setMenuIndex] = useState(0);

  const register = useCallback((id: string, element: HTMLTextAreaElement | null) => {
    if (element) fields.current.set(id, element);
    else fields.current.delete(id);
  }, []);

  // Before paint, not after: restoring focus in a `useEffect` lets the browser show one frame
  // with the caret in the wrong place, which reads as a flicker.
  useLayoutEffect(() => {
    const target = pending.current;
    if (!target) return;
    pending.current = null;

    const field = fields.current.get(target.id);
    if (!field) return;

    field.focus();
    const offset = target.offset === "end" ? field.value.length : target.offset;
    field.setSelectionRange(offset, offset);
  });

  useEffect(() => {
    if (!autoFocus) return;
    const first = blocks[0];
    if (first) fields.current.get(first.id)?.focus();
    // Only on mount: re-running would steal the caret every time the document changed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoFocus]);

  const replace = (index: number, block: Block) => {
    const next = [...blocks];
    next[index] = block;
    onChange(next);
  };

  const setText = (index: number, text: string) => {
    const block = blocks[index];

    // Markdown shortcuts, on paragraphs only. Retyping `- ` inside a bullet should insert a
    // literal dash rather than nesting anything.
    if (block.type === "paragraph") {
      const shortcut = shortcutFor(text);
      if (shortcut) {
        if (shortcut.type === "divider") {
          // A divider holds no text, so the caret goes to a fresh block after it.
          const fresh = newBlock();
          const next = [...blocks];
          next.splice(index, 1, { ...block, type: "divider", text: "" }, fresh);
          pending.current = { id: fresh.id, offset: 0 };
          onChange(next);
          return;
        }
        pending.current = { id: block.id, offset: 0 };
        replace(index, {
          ...block,
          type: shortcut.type,
          text: shortcut.rest,
          ...(shortcut.type === "todo" ? { checked: false } : {}),
        });
        return;
      }
    }

    // `/` on an empty block opens the menu. It stays in the text so backspace closes it
    // naturally, and the block renders as empty because the text is exactly "/".
    if (text === "/" && block.text === "") {
      setMenuFor(block.id);
      setMenuIndex(0);
    } else if (menuFor === block.id && !text.startsWith("/")) {
      setMenuFor(null);
    }

    replace(index, { ...block, text });
  };

  const convert = (index: number, type: BlockType) => {
    const block = blocks[index];
    setMenuFor(null);

    if (type === "divider") {
      const fresh = newBlock();
      const next = [...blocks];
      next.splice(index, 1, { ...block, type: "divider", text: "" }, fresh);
      pending.current = { id: fresh.id, offset: 0 };
      onChange(next);
      return;
    }

    // Drop the "/" that opened the menu.
    const text = block.text.startsWith("/") ? "" : block.text;
    pending.current = { id: block.id, offset: "end" };
    replace(index, {
      ...block,
      type,
      text,
      ...(type === "todo" ? { checked: block.checked ?? false } : {}),
    });
  };

  const splitAt = (index: number, caret: number) => {
    const block = blocks[index];
    const before = block.text.slice(0, caret);
    const after = block.text.slice(caret);

    // Enter on an empty list item ends the list rather than adding a third empty one.
    if (isListItem(block.type) && block.text.trim() === "") {
      pending.current = { id: block.id, offset: 0 };
      replace(index, { ...block, type: "paragraph", text: "" });
      return;
    }

    const fresh = { ...blockAfter(block), text: after };
    const next = [...blocks];
    next.splice(index, 1, { ...block, text: before }, fresh);
    pending.current = { id: fresh.id, offset: 0 };
    onChange(next);
  };

  /**
   * Backspace at the very start of a block.
   *
   * A styled block loses its style first — one press turns a heading back into a paragraph,
   * which is how a mistyped `# ` is undone. A paragraph merges into the block above, with the
   * caret left exactly where the join happened.
   */
  const backspaceAtStart = (index: number) => {
    const block = blocks[index];

    if (block.type !== "paragraph") {
      pending.current = { id: block.id, offset: 0 };
      replace(index, { ...block, type: "paragraph" });
      return;
    }

    if (index === 0) return;

    const previous = blocks[index - 1];
    const next = [...blocks];

    if (previous.type === "divider") {
      // Nothing to merge into; remove the divider and stay put.
      next.splice(index - 1, 1);
      pending.current = { id: block.id, offset: 0 };
      onChange(next);
      return;
    }

    next.splice(index - 1, 2, { ...previous, text: previous.text + block.text });
    pending.current = { id: previous.id, offset: previous.text.length };
    onChange(next);
  };

  const move = (index: number, delta: -1 | 1) => {
    const target = blocks[index + delta];
    if (!target) return;
    pending.current = { id: target.id, offset: delta === -1 ? "end" : 0 };
    // A layout effect does the focusing; nothing about the document changed.
    onChange([...blocks]);
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>, index: number) => {
    const block = blocks[index];
    const field = event.currentTarget;
    const caret = field.selectionStart;
    const hasSelection = field.selectionStart !== field.selectionEnd;

    if (menuFor === block.id) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setMenuIndex((n) => (n + 1) % MENU.length);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setMenuIndex((n) => (n - 1 + MENU.length) % MENU.length);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        convert(index, MENU[menuIndex].type);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setMenuFor(null);
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      // Code blocks keep their newlines; Shift+Enter is the escape hatch everywhere else.
      if (isMultiline(block.type)) return;
      event.preventDefault();
      splitAt(index, caret);
      return;
    }

    if (event.key === "Backspace" && caret === 0 && !hasSelection) {
      event.preventDefault();
      backspaceAtStart(index);
      return;
    }

    // Delete at the very end pulls the next block up — the mirror of backspace.
    if (
      event.key === "Delete" &&
      caret === block.text.length &&
      !hasSelection &&
      index < blocks.length - 1
    ) {
      const following = blocks[index + 1];
      if (following.type === "divider") {
        event.preventDefault();
        const next = [...blocks];
        next.splice(index + 1, 1);
        pending.current = { id: block.id, offset: block.text.length };
        onChange(next);
        return;
      }
      event.preventDefault();
      const next = [...blocks];
      next.splice(index, 2, { ...block, text: block.text + following.text });
      pending.current = { id: block.id, offset: block.text.length };
      onChange(next);
      return;
    }

    // Arrows cross a block boundary only from its edge, so navigating within a wrapped
    // paragraph still works normally.
    if (event.key === "ArrowUp" && caret === 0 && !isMultiline(block.type)) {
      event.preventDefault();
      move(index, -1);
      return;
    }
    if (
      event.key === "ArrowDown" &&
      caret === block.text.length &&
      !isMultiline(block.type)
    ) {
      event.preventDefault();
      move(index, 1);
    }
  };

  return (
    <div className="space-y-0.5">
      {blocks.map((block, index) => (
        <Row
          key={block.id}
          block={block}
          index={index}
          register={register}
          placeholder={index === 0 && blocks.length === 1 ? placeholder : undefined}
          menuOpen={menuFor === block.id}
          menuIndex={menuIndex}
          onPick={(type) => convert(index, type)}
          onCloseMenu={() => setMenuFor(null)}
          onText={(text) => setText(index, text)}
          onToggle={() => replace(index, { ...block, checked: !block.checked })}
          onKeyDown={(event) => onKeyDown(event, index)}
          onFocusHere={() => {
            // A divider has no field; clicking it should put the caret somewhere useful.
            const after = blocks[index + 1] ?? blocks[index - 1];
            if (after) fields.current.get(after.id)?.focus();
          }}
        />
      ))}
    </div>
  );
}

/** Per-type presentation. Kept beside the editor so a new type is one place to change. */
const STYLES: Record<BlockType, string> = {
  paragraph: "text-[14px] leading-relaxed",
  heading1: "text-[22px] font-semibold leading-snug tracking-tight",
  heading2: "text-[18px] font-semibold leading-snug tracking-tight",
  heading3: "text-[15px] font-semibold leading-snug",
  bullet: "text-[14px] leading-relaxed",
  numbered: "text-[14px] leading-relaxed",
  todo: "text-[14px] leading-relaxed",
  quote: "text-[14px] leading-relaxed italic text-ink-muted",
  code: "font-mono text-[12.5px] leading-relaxed",
  divider: "",
};

function Row({
  block,
  index,
  register,
  placeholder,
  menuOpen,
  menuIndex,
  onPick,
  onCloseMenu,
  onText,
  onToggle,
  onKeyDown,
  onFocusHere,
}: {
  block: Block;
  index: number;
  register: (id: string, element: HTMLTextAreaElement | null) => void;
  placeholder?: string;
  menuOpen: boolean;
  menuIndex: number;
  onPick: (type: BlockType) => void;
  onCloseMenu: () => void;
  onText: (text: string) => void;
  onToggle: () => void;
  onKeyDown: (event: React.KeyboardEvent<HTMLTextAreaElement>) => void;
  onFocusHere: () => void;
}) {
  const field = useRef<HTMLTextAreaElement | null>(null);

  // Grow to fit. A textarea does not size to its content, and a fixed height either clips a
  // long paragraph or leaves a gap under a short one.
  useLayoutEffect(() => {
    const element = field.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${element.scrollHeight}px`;
  }, [block.text, block.type]);

  if (block.type === "divider") {
    return (
      <div
        className="group flex cursor-pointer items-center py-2"
        onClick={onFocusHere}
        role="presentation"
      >
        <hr className="w-full border-t border-hairline" />
      </div>
    );
  }

  return (
    <div className="relative flex items-start gap-2">
      {block.type === "bullet" && (
        <span
          className="mt-[9px] h-1.5 w-1.5 shrink-0 rounded-full bg-ink-muted"
          aria-hidden
        />
      )}
      {block.type === "numbered" && (
        <span className="mt-[2px] w-4 shrink-0 text-right text-[14px] tabular-nums text-ink-muted">
          {/* Displayed from position; the file is renumbered the same way on save. */}
          {index + 1}.
        </span>
      )}
      {block.type === "todo" && (
        <button
          type="button"
          onClick={onToggle}
          aria-label={block.checked ? "Mark as not done" : "Mark as done"}
          aria-pressed={block.checked}
          className={`mt-[3px] flex h-4 w-4 shrink-0 items-center justify-center rounded border
                      transition ${
                        block.checked
                          ? "border-transparent bg-accent text-accent-on"
                          : "border-hairline hover:border-ink-muted"
                      }`}
        >
          {block.checked && <Check size={11} strokeWidth={3} aria-hidden />}
        </button>
      )}
      {block.type === "quote" && (
        <span className="mt-1 w-0.5 shrink-0 self-stretch rounded bg-hairline" aria-hidden />
      )}

      <textarea
        ref={(element) => {
          field.current = element;
          register(block.id, element);
        }}
        value={block.text}
        onChange={(event) => onText(event.target.value)}
        onKeyDown={onKeyDown}
        rows={1}
        placeholder={placeholder}
        aria-label={`${block.type} block`}
        spellCheck={block.type !== "code"}
        className={`min-w-0 flex-1 resize-none overflow-hidden bg-transparent outline-none
                    placeholder:text-ink-faint ${STYLES[block.type]} ${
                      block.type === "code" ? "rounded-lg bg-overlay px-3 py-2" : ""
                    } ${block.checked ? "text-ink-faint line-through" : "text-ink"}`}
      />

      {menuOpen && (
        <SlashMenu selected={menuIndex} onPick={onPick} onClose={onCloseMenu} />
      )}
    </div>
  );
}

function SlashMenu({
  selected,
  onPick,
  onClose,
}: {
  selected: number;
  onPick: (type: BlockType) => void;
  onClose: () => void;
}) {
  // Dismiss on an outside click, the way every menu in the app does.
  useEffect(() => {
    const close = () => onClose();
    // Deferred, or the click that opened it closes it in the same tick.
    const id = setTimeout(() => window.addEventListener("mousedown", close), 0);
    return () => {
      clearTimeout(id);
      window.removeEventListener("mousedown", close);
    };
  }, [onClose]);

  return (
    <div
      role="menu"
      className="absolute left-0 top-full z-10 mt-1 w-60 overflow-hidden rounded-xl
                 border border-hairline bg-surface py-1 shadow-dock"
    >
      {MENU.map((item, index) => (
        <button
          key={item.type}
          type="button"
          role="menuitem"
          // `mousedown` rather than `click`: the textarea loses focus on mousedown, and by
          // the time a click fires the caret position this needs is already gone.
          onMouseDown={(event) => {
            event.preventDefault();
            onPick(item.type);
          }}
          className={`flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[13px]
                      transition ${
                        index === selected ? "bg-overlay text-ink" : "text-ink-muted"
                      }`}
        >
          <item.Icon size={14} className="shrink-0" aria-hidden />
          <span className="flex-1">{item.label}</span>
          {item.hint && (
            <span className="font-mono text-[11px] text-ink-faint">{item.hint}</span>
          )}
        </button>
      ))}
    </div>
  );
}
