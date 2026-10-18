/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // Every colour resolves through a CSS variable set by `lib/theme.ts`. A component asks
        // for `bg-surface` and never learns which theme is on, so adding an accent is a row in
        // a table rather than an edit in every file that draws something.
        bg: "var(--bg)",
        surface: "var(--surface)",
        rail: "var(--rail)",
        hairline: "var(--hairline)",
        overlay: "var(--overlay)",

        ink: {
          DEFAULT: "var(--text)",
          muted: "var(--muted)",
          faint: "var(--faint)",
        },

        // Channels rather than `var(--accent)` where a screen uses an opacity modifier
        // (`bg-accent/20`): Tailwind cannot apply one to a bare variable and silently emits no
        // rule at all. `lib/theme.ts` publishes the channels beside the colours.
        accent: {
          DEFAULT: "rgb(var(--accent-rgb) / <alpha-value>)",
          // Text drawn in the accent — timestamps that seek, the selected chip. Written as
          // `text-accent-soft` in several screens and never defined, so it rendered in body text.
          soft: "rgb(var(--accent-rgb) / <alpha-value>)",
          hover: "var(--accent-hover)",
          on: "var(--accent-on)",
        },

        warn: {
          DEFAULT: "var(--warn-bg)",
          text: "rgb(var(--warn-text-rgb) / <alpha-value>)",
          line: "var(--warn-line)",
        },
        ok: {
          DEFAULT: "var(--ok-bg)",
          text: "var(--ok-text)",
          line: "var(--ok-line)",
        },
        danger: {
          DEFAULT: "var(--danger-bg)",
          text: "var(--danger-text)",
          line: "var(--danger-line)",
        },

        // Recording is red in every theme. If it is red, audio is being captured — that has to
        // be true regardless of which accent someone picked.
        record: { DEFAULT: "rgb(var(--record-rgb) / <alpha-value>)", hover: "var(--record-hover)" },
      },
      boxShadow: {
        dock: "0 4px 24px rgba(0,0,0,0.14), 0 1px 3px rgba(0,0,0,0.10)",
      },
    },
  },
  plugins: [],
};
