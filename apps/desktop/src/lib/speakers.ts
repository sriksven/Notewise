/**
 * The rules for naming a voice.
 *
 * Kept apart from the popover that uses them because the interesting part is not the popover.
 * Whether a typed name merges two speakers or renames one decides what the user is told they
 * are about to do, and that decision should be testable without mounting a component.
 *
 * @see MAX_SPEAKER_NAME_CHARS in `core/crates/storage/src/repositories/meeting.rs` — the
 * server enforces the same bound, and this only exists so the user learns about it before
 * pressing Enter rather than after.
 */

import type { Speaker } from "./api";

/** Matches the server's limit. A mismatch here shows as a rejected save with no explanation. */
export const MAX_SPEAKER_NAME_CHARS = 80;

export type NameOutcome =
  | { kind: "empty" }
  | { kind: "unchanged" }
  | { kind: "too-long"; over: number }
  | { kind: "rename"; to: string }
  | { kind: "merge"; to: string; into: Speaker };

/**
 * What saving `typed` would do.
 *
 * Names are compared case-insensitively and trimmed, so typing "dana" when "Dana" already
 * exists merges rather than creating a second Dana that differs only in case. The stored name
 * is what the user typed — matching loosely and storing exactly means they can fix
 * capitalisation without it silently becoming a merge into the old spelling.
 */
export function outcomeOf(
  typed: string,
  current: Speaker,
  all: Speaker[],
): NameOutcome {
  const to = typed.trim();
  if (to.length === 0) return { kind: "empty" };

  const over = to.length - MAX_SPEAKER_NAME_CHARS;
  if (over > 0) return { kind: "too-long", over };

  if (to === current.label) return { kind: "unchanged" };

  const into = all.find(
    (s) => s !== current && s.label !== null && sameName(s.label, to),
  );
  return into ? { kind: "merge", to, into } : { kind: "rename", to };
}

function sameName(a: string, b: string): boolean {
  return a.trim().toLowerCase() === b.trim().toLowerCase();
}

/**
 * Whether this outcome can be saved.
 *
 * A type guard rather than a boolean so the name to save with comes from the check itself.
 * The alternative is reading `outcome.to` behind a separate condition, which typechecks only
 * if the two conditions are kept in agreement by hand.
 */
export function isSavable(
  outcome: NameOutcome,
): outcome is Extract<NameOutcome, { to: string }> {
  return outcome.kind === "rename" || outcome.kind === "merge";
}

/**
 * What to tell the user before they commit.
 *
 * A merge is the one that needs saying out loud: it is not obviously reversible — the two
 * labels become one and the split is gone — so it should never happen without the user having
 * read the word "merge" first.
 */
export function describe(outcome: NameOutcome): string | null {
  switch (outcome.kind) {
    case "merge":
      return `Merges with ${outcome.into.label}, which already has ${countOf(outcome.into)}.`;
    case "too-long":
      return `${outcome.over} character${outcome.over === 1 ? "" : "s"} too long.`;
    case "empty":
    case "unchanged":
    case "rename":
      return null;
  }
}

/** "4 lines" — the weight that makes an anonymous label identifiable. */
export function countOf(speaker: Speaker): string {
  return `${speaker.segments} line${speaker.segments === 1 ? "" : "s"}`;
}

/** How a speaker with no name of their own should be shown. */
export function displayName(label: string | null): string {
  return label ?? "Unattributed";
}
