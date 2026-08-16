import { useEffect } from "react";

/**
 * The keyboard.
 *
 * Deliberately few. Every shortcut here is documented on the Help screen, and the rule is that
 * the two lists cannot drift: a help page describing keys that do nothing is worse than no help
 * page, because it sends someone looking for a fault in their keyboard.
 *
 * Chords are matched on `event.key`, which is the character the layout actually produces. Using
 * `event.code` would bind physical positions and put ⌘N somewhere else entirely on an AZERTY
 * keyboard.
 */

/** Focus whatever search field the current screen has. */
export const FOCUS_SEARCH_EVENT = "notewise:focus-search";

/**
 * When focus was last asked for, or 0.
 *
 * ⌘K may have to navigate first, and the field it wants does not exist until React has
 * rendered the new screen — so the event alone lands on nothing. The request is therefore left
 * standing for a moment and picked up by whichever search box mounts next.
 */
let requestedAt = 0;

/**
 * How long a standing request survives.
 *
 * Long enough for a route change to paint, short enough that a search box opened a minute
 * later for an unrelated reason does not steal the caret out of whatever is being typed.
 */
const REQUEST_TTL_MS = 1_000;

export function requestSearchFocus(): void {
  requestedAt = Date.now();
  window.dispatchEvent(new Event(FOCUS_SEARCH_EVENT));
}

export interface Shortcuts {
  onSearch: () => void;
  onNewNote: () => void;
  onToggleRecording: () => void;
}

/**
 * Whether a keystroke belongs to whatever the user is typing into.
 *
 * Without this, ⌘N inside the notes editor would create a second note mid-sentence. Text
 * fields own their own keyboard; the app only gets what they do not want.
 */
function isTyping(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  if (!element) return false;

  const tag = element.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    element.isContentEditable === true
  );
}

/**
 * Dispatch one keystroke.
 *
 * Exported separately from the hook so it can be tested against real `KeyboardEvent`s without
 * a React renderer. The hook is a `useEffect` around this and nothing else — a test that
 * reimplemented the matching would pass happily while the app did something different.
 */
export function handleKey(event: KeyboardEvent, shortcuts: Shortcuts): void {
  // ⌘ on macOS, Ctrl elsewhere. Both are accepted everywhere rather than sniffing the
  // platform: a user on either muscle memory gets what they expect, and neither chord means
  // anything else here.
  if (!event.metaKey && !event.ctrlKey) return;
  // Option-key chords belong to the OS and to character composition on several layouts.
  if (event.altKey) return;

  const key = event.key.toLowerCase();

  // Search is allowed to interrupt typing — it is how you get *out* of one field and into the
  // search box, and every other application on the machine behaves that way.
  if (key === "k" && !event.shiftKey) {
    event.preventDefault();
    shortcuts.onSearch();
    return;
  }

  if (isTyping(event.target)) return;

  if (key === "n" && !event.shiftKey) {
    event.preventDefault();
    shortcuts.onNewNote();
    return;
  }

  // Shifted, because an unshifted ⌘R is reload in every browser-based shell — including the
  // one Tauri ships — and rebinding it would take away the way out of a wedged window.
  if (key === "r" && event.shiftKey) {
    event.preventDefault();
    shortcuts.onToggleRecording();
  }
}

export function useShortcuts(shortcuts: Shortcuts): void {
  const { onSearch, onNewNote, onToggleRecording } = shortcuts;

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) =>
      handleKey(event, { onSearch, onNewNote, onToggleRecording });

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onSearch, onNewNote, onToggleRecording]);
}

/** Whether a focus request is outstanding and still fresh. */
export function focusIsPending(now = Date.now()): boolean {
  return requestedAt !== 0 && now - requestedAt < REQUEST_TTL_MS;
}

/** For tests, and for a screen that has consumed the request. */
export function clearSearchFocusRequest(): void {
  requestedAt = 0;
}

/**
 * Focus a search input when the shortcut asks for one.
 *
 * An event rather than a ref threaded down through the tree: the field lives on whichever
 * screen is showing, and there may be none at all. Mounting also counts as an opportunity to
 * answer, which is what makes ⌘K work from a screen that has no search box — it navigates, and
 * the box that appears picks the request up.
 */
export function useFocusOnSearch(ref: React.RefObject<HTMLInputElement | null>): void {
  useEffect(() => {
    const focus = () => {
      clearSearchFocusRequest();
      ref.current?.focus();
      ref.current?.select();
    };

    if (focusIsPending()) focus();
    window.addEventListener(FOCUS_SEARCH_EVENT, focus);
    return () => window.removeEventListener(FOCUS_SEARCH_EVENT, focus);
  }, [ref]);
}
