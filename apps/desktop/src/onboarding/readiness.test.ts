import { describe, expect, it } from "vitest";

import {
  bannerProblems,
  canFinish,
  firstUnsatisfied,
  regressions,
  skipConsequences,
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

describe("skipConsequences", () => {
  it("is empty when there is nothing to give up", () => {
    expect(skipConsequences(snapshot())).toEqual([]);
  });

  // The lock-out this exists to prevent: a declined microphone left Finish permanently disabled
  // on the last screen of a first launch, with no other way into the app.
  it("names what a declined microphone costs, rather than blocking", () => {
    const declined = snapshot({
      permissions: {
        satisfied: false,
        required: true,
        microphone: { status: "denied", required: true, detail: null },
        system_audio: { status: "unavailable", required: false, detail: "x" },
      },
    });

    expect(canFinish(declined)).toBe(false);
    expect(skipConsequences(declined)).toEqual([
      "Notewise will not be able to hear a meeting",
    ]);
  });

  it("lists every unfinished step in wizard order", () => {
    const nothing = snapshot({
      model: { satisfied: false, required: true },
      backend: { satisfied: false, required: true },
    });

    expect(skipConsequences(nothing)).toEqual([
      "meetings will not be transcribed",
      "summaries, chat and suggested questions will not run",
    ]);
  });
});

describe("bannerProblems", () => {
  it("is empty when nothing is broken", () => {
    expect(bannerProblems(snapshot())).toEqual([]);
  });

  // What the banner is for: it said "Summaries, chat and suggested questions will not run" while
  // the engine knew Ollama was simply not running.
  it("says what to do, in the engine's words, when the AI backend gives a reason", () => {
    const stopped = snapshot({
      backend: {
        satisfied: false,
        required: true,
        detail: "Ollama isn't running. Open the Ollama app, and Notewise will use it straight away.",
      },
    });

    expect(bannerProblems(stopped)).toEqual([
      "Ollama isn't running. Open the Ollama app, and Notewise will use it straight away. Summaries, chat and suggested questions won't work until then.",
    ]);
  });

  it("finishes a reason that has no full stop", () => {
    const [problem] = bannerProblems(
      snapshot({ backend: { satisfied: false, required: true, detail: "Anthropic needs an API key" } }),
    );
    expect(problem).toMatch(/^Anthropic needs an API key\. Summaries/);
  });

  // An older engine sends no reason. The banner still says where to look.
  it("points at Settings when the engine gives no reason", () => {
    const [problem] = bannerProblems(snapshot({ backend: { satisfied: false, required: true } }));
    expect(problem).toMatch(/Settings → Models/);
  });

  it("says where to fix each broken step, in wizard order", () => {
    const problems = bannerProblems(
      snapshot({
        model: { satisfied: false, required: true },
        backend: { satisfied: false, required: true, detail: "Ollama isn't running." },
      }),
    );

    expect(problems).toHaveLength(2);
    expect(problems[0]).toMatch(/^No transcription model is downloaded.*Settings → Models\.$/);
    expect(problems[1]).toMatch(/^Ollama isn't running\./);
  });
});
