/**
 * The interface's own settings: light or dark, and an accent.
 *
 * Applied as CSS custom properties on `<html>` rather than by swapping Tailwind classes, so a
 * component asks for `bg-surface` once and never learns which theme is on. Adding a colour is
 * then a row in a table here, not an edit in every file that draws something.
 */

export type Mode = "light" | "dark" | "system";

export interface Accent {
  id: string;
  label: string;
  /** For the light theme. */
  base: string;
  hover: string;
  /**
   * For the dark theme, lighter.
   *
   * Not a nicety: the same colour cannot serve both. Graphite at `#3f3f46` is a readable button
   * on white and disappears entirely against a `#141416` background — the logo and every
   * primary action would be a dark shape on a dark surface.
   */
  darkBase: string;
  darkHover: string;
  /** Text drawn on top of the accent. Explicit because a mid-tone accent needs dark text. */
  on: string;
  darkOn: string;
}

/**
 * Eleven accents, deliberately muted.
 *
 * This is an app someone stares at for the length of a meeting, so these are desaturated rather
 * than vivid — a saturated accent is legible for a minute and tiring for an hour. Each one is
 * checked to hold contrast against both the light and dark surfaces below.
 */
export const ACCENTS: Accent[] = [
  { id: "graphite", label: "Graphite", base: "#3f3f46", hover: "#27272a", darkBase: "#d4d4d8", darkHover: "#e4e4e7", on: "#ffffff", darkOn: "#18181b" },
  { id: "crimson", label: "Crimson", base: "#e2382e", hover: "#c92f26", darkBase: "#f0665d", darkHover: "#f47c74", on: "#ffffff", darkOn: "#1a0e0d" },
  { id: "ember", label: "Ember", base: "#d4622a", hover: "#b8501f", darkBase: "#e8874f", darkHover: "#ee9c6b", on: "#ffffff", darkOn: "#1a0f08" },
  { id: "amber", label: "Amber", base: "#c08a1e", hover: "#a37416", darkBase: "#e0b85f", darkHover: "#e8c877", on: "#ffffff", darkOn: "#1a1408" },
  { id: "moss", label: "Moss", base: "#4f7d4a", hover: "#3f663b", darkBase: "#7fb377", darkHover: "#94c48c", on: "#ffffff", darkOn: "#0d1a0c" },
  { id: "teal", label: "Teal", base: "#2f7d78", hover: "#256662", darkBase: "#5fb3ad", darkHover: "#7bc4bf", on: "#ffffff", darkOn: "#081a19" },
  { id: "ocean", label: "Ocean", base: "#2f6ba8", hover: "#25568a", darkBase: "#6aa5db", darkHover: "#84b6e4", on: "#ffffff", darkOn: "#081320" },
  { id: "indigo", label: "Indigo", base: "#4f56a8", hover: "#40468a", darkBase: "#8b91dd", darkHover: "#a1a6e6", on: "#ffffff", darkOn: "#0d0f20" },
  { id: "violet", label: "Violet", base: "#7a4fa8", hover: "#63408a", darkBase: "#b088dd", darkHover: "#c1a0e6", on: "#ffffff", darkOn: "#150d20" },
  { id: "plum", label: "Plum", base: "#a04a7d", hover: "#853c67", darkBase: "#d283b3", darkHover: "#dd9cc3", on: "#ffffff", darkOn: "#1d0d17" },
  { id: "slate", label: "Slate", base: "#5a6b7d", hover: "#485666", darkBase: "#9fb0c2", darkHover: "#b3c1d0", on: "#ffffff", darkOn: "#0e1319" },
];

export const DEFAULT_ACCENT = "graphite";

/** Surfaces, text and lines for one mode. */
interface Palette {
  bg: string;
  surface: string;
  rail: string;
  text: string;
  muted: string;
  faint: string;
  hairline: string;
  overlay: string;
  /** Warning, success and danger, as a surface / text / border triple each. */
  warnBg: string;
  warnText: string;
  warnLine: string;
  okBg: string;
  okText: string;
  okLine: string;
  dangerBg: string;
  dangerText: string;
  dangerLine: string;
}

const LIGHT: Palette = {
  bg: "#ffffff",
  surface: "#ffffff",
  rail: "#fbfbfc",
  text: "#18181b",
  muted: "#52525b",
  faint: "#a1a1aa",
  hairline: "#e9e9ec",
  overlay: "rgba(0,0,0,0.06)",
  warnBg: "#fdf6e7",
  warnText: "#7a5804",
  warnLine: "#f0dfb0",
  okBg: "#eef7ef",
  okText: "#2f6b38",
  okLine: "#c9e3ce",
  dangerBg: "#fdeeed",
  dangerText: "#9c2b23",
  dangerLine: "#f3cbc8",
};

/**
 * Not black. A true `#000` background against light text produces halation — the text appears
 * to smear — and is the single most common mistake in a dark theme. `#141416` reads as black
 * and does not.
 */
const DARK: Palette = {
  bg: "#141416",
  surface: "#1b1b1e",
  rail: "#191a1c",
  text: "#ededf0",
  muted: "#a1a1aa",
  faint: "#71717a",
  hairline: "#2a2a2e",
  overlay: "rgba(255,255,255,0.07)",
  // Tinted surfaces rather than the light theme's pastels, which glow on a dark
  // background. Text carries the meaning; the fill only groups it.
  warnBg: "#2a2312",
  warnText: "#e8c877",
  warnLine: "#4a3c1c",
  okBg: "#152618",
  okText: "#8fd39c",
  okLine: "#26412c",
  dangerBg: "#2b1715",
  dangerText: "#f0a49d",
  dangerLine: "#4d2622",
};

export interface Theme {
  mode: Mode;
  accent: string;
}

export const DEFAULT_THEME: Theme = { mode: "dark", accent: DEFAULT_ACCENT };

/** Whether `system` currently means dark. */
export function prefersDark(): boolean {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
}

export function resolveMode(mode: Mode): "light" | "dark" {
  return mode === "system" ? (prefersDark() ? "dark" : "light") : mode;
}

/**
 * Write a theme onto the document.
 *
 * Idempotent, and safe to call on every change — setting a custom property that already holds
 * the same value does not cause a repaint.
 */
export function applyTheme(theme: Theme): void {
  const resolved = resolveMode(theme.mode);
  const palette = resolved === "dark" ? DARK : LIGHT;
  const accent = ACCENTS.find((a) => a.id === theme.accent) ?? ACCENTS[0];

  const root = document.documentElement;
  root.style.setProperty("--bg", palette.bg);
  root.style.setProperty("--surface", palette.surface);
  root.style.setProperty("--rail", palette.rail);
  root.style.setProperty("--text", palette.text);
  root.style.setProperty("--muted", palette.muted);
  root.style.setProperty("--faint", palette.faint);
  root.style.setProperty("--hairline", palette.hairline);
  root.style.setProperty("--overlay", palette.overlay);
  root.style.setProperty("--warn-bg", palette.warnBg);
  root.style.setProperty("--warn-text", palette.warnText);
  root.style.setProperty("--warn-line", palette.warnLine);
  root.style.setProperty("--ok-bg", palette.okBg);
  root.style.setProperty("--ok-text", palette.okText);
  root.style.setProperty("--ok-line", palette.okLine);
  root.style.setProperty("--danger-bg", palette.dangerBg);
  root.style.setProperty("--danger-text", palette.dangerText);
  root.style.setProperty("--danger-line", palette.dangerLine);
  const dark = resolved === "dark";
  root.style.setProperty("--accent", dark ? accent.darkBase : accent.base);
  root.style.setProperty("--accent-hover", dark ? accent.darkHover : accent.hover);
  root.style.setProperty("--accent-on", dark ? accent.darkOn : accent.on);

  // Recording is always red, in every theme. It is the one signal that must not be confusable
  // with decoration — if it is red, audio is being captured — so it does not follow the accent.
  root.style.setProperty("--record", "#e2382e");
  root.style.setProperty("--record-hover", "#c92f26");

  root.dataset.mode = resolved;
  // Tells the browser to draw native scrollbars and form controls to match.
  root.style.colorScheme = resolved;
}
