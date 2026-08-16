import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  clearSearchFocusRequest,
  focusIsPending,
  FOCUS_SEARCH_EVENT,
  handleKey,
  requestSearchFocus,
} from "./shortcuts";

/**
 * These tests run the real `handleKey`, not a copy of its logic.
 *
 * The event is a plain object rather than a `KeyboardEvent` because this project's tests have
 * no DOM environment, and adding one to assert on six boolean fields would be a dependency
 * bought for nothing. `handleKey` reads exactly the properties below and calls
 * `preventDefault`; anything it started reading that is not here would fail loudly as
 * `undefined` rather than pass quietly.
 */
interface Press {
  meta?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  /** The element the keystroke landed in, as a tag name. */
  in?: string;
  contentEditable?: boolean;
}

let shortcuts: {
  onSearch: ReturnType<typeof vi.fn>;
  onNewNote: ReturnType<typeof vi.fn>;
  onToggleRecording: ReturnType<typeof vi.fn>;
};

beforeEach(() => {
  shortcuts = {
    onSearch: vi.fn(),
    onNewNote: vi.fn(),
    onToggleRecording: vi.fn(),
  };
});

function press(key: string, options: Press = {}): { prevented: boolean } {
  let prevented = false;

  const target = options.in
    ? { tagName: options.in.toUpperCase(), isContentEditable: options.contentEditable === true }
    : { tagName: "BODY", isContentEditable: false };

  const event = {
    key,
    metaKey: options.meta ?? false,
    ctrlKey: options.ctrl ?? false,
    shiftKey: options.shift ?? false,
    altKey: options.alt ?? false,
    target,
    preventDefault: () => {
      prevented = true;
    },
  } as unknown as KeyboardEvent;

  handleKey(event, shortcuts);
  return { prevented };
}

describe("handleKey", () => {
  it("opens search on the command key and on control", () => {
    press("k", { meta: true });
    press("k", { ctrl: true });
    expect(shortcuts.onSearch).toHaveBeenCalledTimes(2);
  });

  it("makes a note on ⌘N", () => {
    press("n", { meta: true });
    expect(shortcuts.onNewNote).toHaveBeenCalledOnce();
  });

  it("is case-insensitive, so caps lock does not disable it", () => {
    press("N", { meta: true });
    expect(shortcuts.onNewNote).toHaveBeenCalledOnce();
  });

  it("needs shift for recording, because ⌘R reloads the window", () => {
    press("r", { meta: true });
    expect(shortcuts.onToggleRecording).not.toHaveBeenCalled();

    press("r", { meta: true, shift: true });
    expect(shortcuts.onToggleRecording).toHaveBeenCalledOnce();
  });

  it("ignores a bare keypress", () => {
    press("n");
    press("k");
    expect(shortcuts.onNewNote).not.toHaveBeenCalled();
    expect(shortcuts.onSearch).not.toHaveBeenCalled();
  });

  // Option-key chords belong to the OS and to character composition on several layouts.
  it("ignores anything with option held", () => {
    press("n", { meta: true, alt: true });
    press("k", { meta: true, alt: true });
    expect(shortcuts.onNewNote).not.toHaveBeenCalled();
    expect(shortcuts.onSearch).not.toHaveBeenCalled();
  });

  // The bug this prevents: ⌘N inside the notes editor creating a second note mid-sentence.
  it("leaves text fields alone", () => {
    for (const tag of ["input", "textarea", "select"]) {
      press("n", { meta: true, in: tag });
      press("r", { meta: true, shift: true, in: tag });
    }
    expect(shortcuts.onNewNote).not.toHaveBeenCalled();
    expect(shortcuts.onToggleRecording).not.toHaveBeenCalled();
  });

  it("leaves a contenteditable alone", () => {
    press("n", { meta: true, in: "div", contentEditable: true });
    expect(shortcuts.onNewNote).not.toHaveBeenCalled();
  });

  // Search is the exception: it is how you get *out* of a field.
  it("still opens search from inside a text field", () => {
    press("k", { meta: true, in: "input" });
    expect(shortcuts.onSearch).toHaveBeenCalledOnce();
  });

  it("prevents the default only for the chords it claims", () => {
    expect(press("k", { meta: true }).prevented).toBe(true);
    expect(press("n", { meta: true }).prevented).toBe(true);
    expect(press("r", { meta: true, shift: true }).prevented).toBe(true);

    // Reload must still reload.
    expect(press("r", { meta: true }).prevented).toBe(false);
    // And an unclaimed chord must reach the shell.
    expect(press("p", { meta: true }).prevented).toBe(false);
  });

  it("does not swallow a keystroke it is going to ignore anyway", () => {
    expect(press("n", { meta: true, in: "input" }).prevented).toBe(false);
  });
});

describe("the focus request", () => {
  // `requestSearchFocus` also dispatches on `window`, for a field already on screen. These
  // tests are about the standing request, so the dispatch target is a stub — there is no DOM
  // here, and the listener side is exercised in the browser run instead.
  beforeEach(() => {
    clearSearchFocusRequest();
    vi.stubGlobal("window", { dispatchEvent: vi.fn() });
    vi.stubGlobal("Event", class {});
  });

  afterEach(() => vi.unstubAllGlobals());

  it("is namespaced, so it cannot collide with a library's", () => {
    expect(FOCUS_SEARCH_EVENT).toBe("notewise:focus-search");
  });

  it("is not pending until something asks", () => {
    expect(focusIsPending()).toBe(false);
  });

  /**
   * The reason a plain event was not enough: ⌘K from a screen with no search box has to
   * navigate first, and the field does not exist until React renders the new screen. The
   * request stands so the field can pick it up when it mounts.
   */
  it("stands after being made, so a field that mounts later can answer it", () => {
    requestSearchFocus();
    expect(focusIsPending()).toBe(true);
  });

  // Otherwise a search box opened a minute later for an unrelated reason would steal the
  // caret out of whatever the user had started typing.
  it("expires rather than waiting forever", () => {
    requestSearchFocus();
    expect(focusIsPending(Date.now() + 999)).toBe(true);
    expect(focusIsPending(Date.now() + 5_000)).toBe(false);
  });

  it("is cleared once answered", () => {
    requestSearchFocus();
    clearSearchFocusRequest();
    expect(focusIsPending()).toBe(false);
  });
});
