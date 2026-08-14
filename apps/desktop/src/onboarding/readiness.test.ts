import { describe, expect, it } from "vitest";

import {
  canFinish,
  firstUnsatisfied,
  regressions,
  stepsFor,
  type SetupReadiness,
} from "./readiness";

/** A snapshot with everything satisfied, narrowed per test. */
function snapshot(overrides: Partial<SetupReadiness["steps"]> = {}): SetupReadiness {
  return {
    completed_at: null,
    steps: {
      model: { satisfied: true, required: true },
      backend: { satisfied: true, required: true },
      permissions: {
        satisfied: true,
        required: true,
        microphone: { status: "granted", required: true, detail: null },
        system_audio: { status: "unavailable", required: false, detail: "no signed bundle" },
      },
      ...overrides,
    },
  };
}

describe("stepsFor", () => {
  it("always offers all four steps, welcome first", () => {
    expect(stepsFor(snapshot()).map((s) => s.id)).toEqual([
      "welcome",
      "model",
      "backend",
      "permissions",
    ]);
  });

  it("marks welcome satisfied so it never blocks finishing", () => {
    const welcome = stepsFor(snapshot()).find((s) => s.id === "welcome");
    expect(welcome?.satisfied).toBe(true);
  });
});

describe("firstUnsatisfied", () => {
  it("lands on model when nothing is downloaded", () => {
    expect(firstUnsatisfied(snapshot({ model: { satisfied: false, required: true } }))).toBe(
      "model",
    );
  });

  it("skips satisfied steps and lands on permissions", () => {
    const next = firstUnsatisfied(
      snapshot({
        permissions: {
          satisfied: false,
          required: true,
          microphone: { status: "not_requested", required: true, detail: null },
          system_audio: { status: "unavailable", required: false, detail: "x" },
        },
      }),
    );
    expect(next).toBe("permissions");
  });

  // Next from Welcome jumps to the first unsatisfied step. With nothing left to do it must
  // land on the last step rather than fall off the end, so Finish stays reachable.
  it("lands on the last step when everything is already satisfied", () => {
    expect(firstUnsatisfied(snapshot())).toBe("permissions");
  });
});

describe("canFinish", () => {
  it("is true when every required step is satisfied", () => {
    expect(canFinish(snapshot())).toBe(true);
  });

  it("is false when a required step is unsatisfied", () => {
    expect(canFinish(snapshot({ model: { satisfied: false, required: true } }))).toBe(false);
  });

  // The rule the whole "required only when available" decision rests on: a capability nobody
  // can grant must not be able to block the button forever.
  it("ignores an unavailable capability", () => {
    const withUnavailableSystemAudio = snapshot({
      permissions: {
        satisfied: true,
        required: true,
        microphone: { status: "granted", required: true, detail: null },
        system_audio: {
          status: "unavailable",
          required: false,
          detail: "ScreenCaptureKit requires a signed bundle",
        },
      },
    });
    expect(canFinish(withUnavailableSystemAudio)).toBe(true);
  });

  it("ignores a step that is not required, even when unsatisfied", () => {
    // Pinned so a future edit does not quietly turn `required: false` into a hidden gate.
    expect(canFinish(snapshot({ backend: { satisfied: false, required: false } }))).toBe(true);
  });
});

describe("regressions", () => {
  it("names what broke after setup completed", () => {
    const completed: SetupReadiness = {
      ...snapshot({ model: { satisfied: false, required: true } }),
      completed_at: "2026-08-13T10:00:00Z",
    };

    expect(regressions(completed).map((s) => s.id)).toEqual(["model"]);
    expect(canFinish(completed)).toBe(false);
  });

  it("is empty when nothing has regressed", () => {
    expect(regressions(snapshot())).toEqual([]);
  });
});
