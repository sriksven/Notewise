import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, Merge, UserRound } from "lucide-react";

import type { Speaker } from "../lib/api";
import { duration } from "../lib/format";
import {
  countOf,
  describe,
  displayName,
  isSavable,
  MAX_SPEAKER_NAME_CHARS,
  outcomeOf,
} from "../lib/speakers";

/** Fixed rather than fluid, because the position is measured against it before it renders. */
const POPOVER_WIDTH = 288;
/** Breathing room between the name and its popover. */
const GAP = 6;
/** Closest the popover may sit to the edge of the window. */
const MARGIN = 8;

interface Props {
  speaker: Speaker;
  /** Everyone in this meeting, so a typed name can be recognised as a merge. */
  all: Speaker[];
  onRename: (from: string | null, to: string) => Promise<void>;
  /** False while a meeting is recording — see the note on the trigger below. */
  editable: boolean;
}

/**
 * A speaker's name in the transcript, clickable to correct it.
 *
 * # Why the transcript and not a settings panel
 *
 * "Speaker 2 is Dana" is a realisation someone has while reading what Speaker 2 said. Sending
 * them to a panel to act on it means holding the mapping in their head on the way there, and
 * the panel cannot show the one thing that identifies an anonymous cluster — its words.
 *
 * # Merging is a rename
 *
 * Typing a name another speaker already has folds the two together, which is the repair for a
 * diarizer having split one person in two. The popover says so before it happens, because a
 * merge discards the split and re-separating them is not something this UI can offer.
 */
export function SpeakerName({ speaker, all, onRename, editable }: Props) {
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [at, setAt] = useState({ top: 0, left: 0 });

  const outcome = outcomeOf(typed, speaker, all);
  const note = describe(outcome);
  const others = all.filter((s) => s !== speaker && s.label !== null);

  // Start empty for an anonymous cluster: "Speaker 2" is not a draft of the person's name, and
  // pre-filling it means every rename begins by clearing it.
  useLayoutEffect(() => {
    if (!open) return;
    setTyped(speaker.anonymous ? "" : (speaker.label ?? ""));
    setError(null);
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [open, speaker]);

  /**
   * Pin the popover to the trigger in viewport coordinates.
   *
   * The transcript scrolls, and a scroll container clips its overflow on *both* axes — so an
   * absolutely-positioned popover inside it gets cut off at the column edge no matter what its
   * z-index is. Rendering into `document.body` and positioning by measurement is what makes it
   * whole, and it is also what lets it flip rather than run off the window.
   */
  const place = useCallback(() => {
    const trigger = triggerRef.current?.getBoundingClientRect();
    if (!trigger) return;

    const height = popoverRef.current?.offsetHeight ?? 240;
    const below = trigger.bottom + GAP;
    const flip = below + height > window.innerHeight - MARGIN && trigger.top > height + MARGIN;

    setAt({
      top: flip ? trigger.top - height - GAP : below,
      // Clamped, so a speaker named near the right edge does not open off-screen.
      left: Math.max(
        MARGIN,
        Math.min(trigger.left, window.innerWidth - POPOVER_WIDTH - MARGIN),
      ),
    });
  }, []);

  useLayoutEffect(() => {
    if (open) place();
  }, [open, place, typed, all.length]);

  useEffect(() => {
    if (!open) return;

    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (popoverRef.current?.contains(target) || triggerRef.current?.contains(target)) return;
      setOpen(false);
    };
    // Capture, so a click landing on another speaker's name closes this one before opening that.
    document.addEventListener("pointerdown", onPointerDown, true);
    // Capture again: the transcript scrolls, not the window, so a bubbling listener on `window`
    // never hears it and the popover would drift away from the name it belongs to.
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [open, place]);

  async function save(to: string) {
    setSaving(true);
    setError(null);
    try {
      await onRename(speaker.label, to);
      setOpen(false);
    } catch (e) {
      // Most likely a stale label — someone renamed it in another window. Say so rather than
      // closing, so the typed name is not lost.
      setError(e instanceof Error ? e.message : "Could not rename this speaker.");
    } finally {
      setSaving(false);
    }
  }

  if (!editable) {
    return <span className="text-[13px] font-semibold text-ink">{displayName(speaker.label)}</span>;
  }

  return (
    <span className="inline-flex">
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((wasOpen) => !wasOpen)}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={`Name this speaker — ${countOf(speaker)}, ${duration(speaker.speaking_ms)}`}
        className={`rounded-md px-1 -mx-1 text-[13px] font-semibold transition-colors
                    hover:bg-overlay hover:text-ink
                    ${speaker.anonymous ? "text-ink-muted decoration-dotted underline underline-offset-4" : "text-ink"}`}
      >
        {displayName(speaker.label)}
      </button>

      {open && createPortal(
        <div
          ref={popoverRef}
          role="dialog"
          aria-label={`Name ${displayName(speaker.label)}`}
          style={{ top: at.top, left: at.left, width: POPOVER_WIDTH }}
          className="fixed z-50 rounded-xl border border-hairline bg-surface p-3 shadow-2xl"
        >
          <p className="mb-2 text-[11.5px] text-ink-faint">
            {countOf(speaker)} · {duration(speaker.speaking_ms)} of speech
          </p>

          <input
            ref={inputRef}
            value={typed}
            maxLength={MAX_SPEAKER_NAME_CHARS + 20}
            placeholder="Who is this?"
            onChange={(e) => setTyped(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && isSavable(outcome)) {
                e.preventDefault();
                void save(outcome.to);
              } else if (e.key === "Escape") {
                e.preventDefault();
                setOpen(false);
              }
            }}
            className="w-full rounded-lg border border-hairline bg-bg px-2.5 py-1.5 text-[13px]
                       text-ink outline-none placeholder:text-ink-faint focus:border-accent"
          />

          {note && (
            <p
              className={`mt-2 text-[11.5px] leading-snug ${
                outcome.kind === "merge" ? "text-accent" : "text-danger-text"
              }`}
            >
              {note}
            </p>
          )}
          {error && <p className="mt-2 text-[11.5px] leading-snug text-danger-text">{error}</p>}

          <div className="mt-2.5 flex items-center gap-2">
            <button
              type="button"
              disabled={!isSavable(outcome) || saving}
              onClick={() => isSavable(outcome) && void save(outcome.to)}
              className="inline-flex items-center gap-1.5 rounded-lg bg-accent px-2.5 py-1.5
                         text-[12px] font-medium text-accent-on transition-opacity
                         hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
            >
              {outcome.kind === "merge" ? <Merge size={13} /> : <Check size={13} />}
              {saving ? "Saving…" : outcome.kind === "merge" ? "Merge" : "Save"}
            </button>
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="rounded-lg px-2 py-1.5 text-[12px] text-ink-muted hover:text-ink"
            >
              Cancel
            </button>
          </div>

          {others.length > 0 && (
            <div className="mt-3 border-t border-hairline pt-2.5">
              <p className="mb-1.5 text-[11px] text-ink-faint">
                Or merge into someone already here
              </p>
              <div className="flex flex-col gap-0.5">
                {others.map((other) => (
                  <button
                    key={other.label}
                    type="button"
                    disabled={saving}
                    onClick={() => void save(other.label as string)}
                    className="flex items-center gap-2 rounded-lg px-1.5 py-1 text-left text-[12px]
                               text-ink-muted transition-colors hover:bg-overlay hover:text-ink
                               disabled:opacity-40"
                  >
                    <UserRound size={12} className="shrink-0 text-ink-faint" aria-hidden />
                    <span className="truncate">{other.label}</span>
                    <span className="ml-auto shrink-0 text-[11px] text-ink-faint">
                      {countOf(other)}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>,
        document.body,
      )}
    </span>
  );
}
