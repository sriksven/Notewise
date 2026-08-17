import { describe as group, expect, it } from "vitest";

import type { Speaker } from "./api";
import {
  countOf,
  describe,
  displayName,
  isSavable,
  MAX_SPEAKER_NAME_CHARS,
  outcomeOf,
} from "./speakers";

function speaker(label: string | null, segments = 1): Speaker {
  return {
    label,
    segments,
    speaking_ms: segments * 1000,
    first_at_ms: 0,
    anonymous: label === null || /^Speaker \d+$/.test(label),
  };
}

group("outcomeOf", () => {
  it("names an anonymous cluster", () => {
    const one = speaker("Speaker 1");
    expect(outcomeOf("Dana", one, [one])).toEqual({ kind: "rename", to: "Dana" });
  });

  it("merges when the name belongs to another speaker", () => {
    const three = speaker("Speaker 3");
    const dana = speaker("Dana", 4);

    const outcome = outcomeOf("Dana", three, [dana, three]);
    expect(outcome).toMatchObject({ kind: "merge", to: "Dana" });
    if (outcome.kind === "merge") expect(outcome.into).toBe(dana);
  });

  // Otherwise "dana" and "Dana" coexist as two people who are one person.
  it("merges regardless of case", () => {
    const three = speaker("Speaker 3");
    const dana = speaker("Dana", 4);
    expect(outcomeOf("  dANa ", three, [dana, three]).kind).toBe("merge");
  });

  // Fixing capitalisation must stay a rename, or it silently reverts to the old spelling.
  it("treats recapitalising your own name as a rename", () => {
    const dana = speaker("dana", 4);
    expect(outcomeOf("Dana", dana, [dana])).toEqual({ kind: "rename", to: "Dana" });
  });

  it("reports an unchanged name rather than saving it", () => {
    const dana = speaker("Dana");
    expect(outcomeOf("Dana", dana, [dana]).kind).toBe("unchanged");
    expect(outcomeOf("  Dana  ", dana, [dana]).kind).toBe("unchanged");
  });

  it("rejects a blank name", () => {
    const one = speaker("Speaker 1");
    expect(outcomeOf("   ", one, [one]).kind).toBe("empty");
  });

  it("reports how far over the limit a name is", () => {
    const one = speaker("Speaker 1");
    const outcome = outcomeOf("n".repeat(MAX_SPEAKER_NAME_CHARS + 3), one, [one]);
    expect(outcome).toEqual({ kind: "too-long", over: 3 });
  });

  it("names the unattributed group", () => {
    const nobody = speaker(null, 2);
    expect(outcomeOf("Priya", nobody, [nobody])).toEqual({
      kind: "rename",
      to: "Priya",
    });
  });

  // A meeting's own null-labelled group is not a merge target — there is no name to merge into.
  it("does not merge into the unattributed group", () => {
    const nobody = speaker(null, 2);
    const one = speaker("Speaker 1");
    expect(outcomeOf("Priya", one, [nobody, one]).kind).toBe("rename");
  });
});

group("isSavable", () => {
  it("allows renames and merges only", () => {
    expect(isSavable({ kind: "rename", to: "Dana" })).toBe(true);
    expect(isSavable({ kind: "merge", to: "Dana", into: speaker("Dana") })).toBe(true);
    expect(isSavable({ kind: "empty" })).toBe(false);
    expect(isSavable({ kind: "unchanged" })).toBe(false);
    expect(isSavable({ kind: "too-long", over: 1 })).toBe(false);
  });
});

group("describe", () => {
  // A merge is not obviously reversible, so it must be said out loud before it happens.
  it("warns about a merge and names what it merges into", () => {
    const dana = speaker("Dana", 4);
    const text = describe({ kind: "merge", to: "Dana", into: dana });
    expect(text).toContain("Merges with Dana");
    expect(text).toContain("4 lines");
  });

  it("says nothing about an ordinary rename", () => {
    expect(describe({ kind: "rename", to: "Dana" })).toBeNull();
  });

  it("pluralises the overflow count", () => {
    expect(describe({ kind: "too-long", over: 1 })).toBe("1 character too long.");
    expect(describe({ kind: "too-long", over: 2 })).toBe("2 characters too long.");
  });
});

group("countOf", () => {
  it("pluralises lines", () => {
    expect(countOf(speaker("Dana", 1))).toBe("1 line");
    expect(countOf(speaker("Dana", 4))).toBe("4 lines");
  });
});

group("displayName", () => {
  it("gives the unlabelled group a name to click on", () => {
    expect(displayName(null)).toBe("Unattributed");
    expect(displayName("Dana")).toBe("Dana");
  });
});
