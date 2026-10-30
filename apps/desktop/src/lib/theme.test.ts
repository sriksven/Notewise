import { describe, expect, it } from "vitest";

import { ACCENTS, DEFAULT_ACCENT, DEFAULT_THEME, channels, companionAccent, resolveMode } from "./theme";
import tailwindConfig from "../../tailwind.config.js?raw";
import themeSource from "./theme.ts?raw";

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

describe("companionAccent", () => {
  // The note's Ask is drawn in it so it never looks like the workspace Ask beside it. A companion
  // equal to the accent would put the two back in one colour.
  it("is never the accent it accompanies", () => {
    for (const accent of ACCENTS) {
      expect(companionAccent(accent.id).id, accent.id).not.toBe(accent.id);
    }
  });

  it("still gives a colour for an accent this build does not know", () => {
    expect(ACCENTS).toContain(companionAccent("retired-accent"));
  });

  // `index.css` carries the default's companion as its fallback, for the moment before
  // `applyTheme` runs. If the default changes, that fallback has to change with it.
  it("pairs the default accent with teal", () => {
    expect(companionAccent(DEFAULT_ACCENT).id).toBe("teal");
  });
});

describe("resolveMode", () => {
  it("passes an explicit choice through", () => {
    expect(resolveMode("dark")).toBe("dark");
    expect(resolveMode("light")).toBe("light");
  });
});

describe("channels", () => {
  it("writes a hex colour as rgb() channels", () => {
    expect(channels("#3f3f46")).toBe("63 63 70");
    expect(channels("#e2382e")).toBe("226 56 46");
    expect(channels("#ffffff")).toBe("255 255 255");
  });

  // The bug: colours given to Tailwind as `var(--accent)` cannot take an opacity modifier, so
  // `bg-accent/[0.06]` (the selected meeting) and `bg-record/10` (the go-live button) compiled to
  // no rule at all. The config now reads channel variables — and every one it reads has to be
  // set by `applyTheme`, or those classes turn transparent again. (The fallbacks in `index.css`
  // cannot be checked from here: vitest hands a stylesheet over as an empty string, `?raw` or not.)
  it("are published for every channel variable the Tailwind config reads", () => {
    const read = [...tailwindConfig.matchAll(/var\((--[a-z-]+-rgb)\)/g)].map((m) => m[1]);
    expect(read.length).toBeGreaterThan(0);
    for (const name of new Set(read)) {
      expect(themeSource, `applyTheme sets ${name}`).toContain(`"${name}"`);
    }
  });
});
