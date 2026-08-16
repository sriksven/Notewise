import { describe, expect, it } from "vitest";

import { ACCENTS, DEFAULT_ACCENT, DEFAULT_THEME, resolveMode } from "./theme";

const HEX = /^#[0-9a-f]{6}$/;

describe("accents", () => {
  // Written by hand into a table, which is exactly where a typo survives review and then
  // renders as a transparent button nobody can see.
  it("are all valid six-digit hex", () => {
    for (const accent of ACCENTS) {
      for (const field of ["base", "hover", "darkBase", "darkHover", "on", "darkOn"] as const) {
        expect(accent[field], `${accent.id}.${field}`).toMatch(HEX);
      }
    }
  });

  it("offers eleven distinct choices", () => {
    expect(ACCENTS).toHaveLength(11);
    expect(new Set(ACCENTS.map((a) => a.id)).size).toBe(11);
  });

  /**
   * The bug this exists for: graphite is a readable button on white and invisible on a
   * `#141416` background. A dark variant that is not lighter than its light one is the same
   * mistake written down.
   */
  it("has a lighter variant for the dark theme", () => {
    const luminance = (hex: string) => {
      const n = parseInt(hex.slice(1), 16);
      return 0.2126 * ((n >> 16) & 255) + 0.7152 * ((n >> 8) & 255) + 0.0722 * (n & 255);
    };

    for (const accent of ACCENTS) {
      expect(
        luminance(accent.darkBase),
        `${accent.id} must be lighter in dark mode than in light`,
      ).toBeGreaterThan(luminance(accent.base));
    }
  });

  it("defaults to an accent that exists", () => {
    expect(ACCENTS.some((a) => a.id === DEFAULT_ACCENT)).toBe(true);
    expect(ACCENTS.some((a) => a.id === DEFAULT_THEME.accent)).toBe(true);
  });
});

describe("resolveMode", () => {
  it("passes an explicit choice through", () => {
    expect(resolveMode("dark")).toBe("dark");
    expect(resolveMode("light")).toBe("light");
  });
});
