/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./download/index.html", "./src/**/*.{ts,js}"],
  theme: {
    extend: {
      colors: {
        // The desktop app's dark palette, so the site and the product do not look like two
        // different things. Literal values rather than CSS variables: there is no theme
        // switcher here, and a landing page should paint correctly before any script runs.
        ink: {
          DEFAULT: "#ededf0",
          muted: "#a1a1aa",
          faint: "#71717a",
        },
        bg: "#0d0d0f",
        surface: "#141416",
        raised: "#1b1b1e",
        hairline: "#2a2a2e",
        record: "#e2382e",
        accent: {
          DEFAULT: "#7c8cf8",
          soft: "#a5b0fa",
        },
      },
      fontFamily: {
        sans: [
          "-apple-system",
          "BlinkMacSystemFont",
          "Segoe UI",
          "Roboto",
          "Helvetica Neue",
          "Arial",
          "sans-serif",
        ],
        mono: ["ui-monospace", "SFMono-Regular", "Menlo", "monospace"],
      },
      keyframes: {
        "fade-up": {
          from: { opacity: "0", transform: "translateY(24px)" },
          to: { opacity: "1", transform: "none" },
        },
        drift: {
          "0%, 100%": { transform: "translate3d(0,0,0)" },
          "50%": { transform: "translate3d(0,-14px,0)" },
        },
        "pulse-ring": {
          "0%": { boxShadow: "0 0 0 0 rgba(226,56,46,0.45)" },
          "70%": { boxShadow: "0 0 0 18px rgba(226,56,46,0)" },
          "100%": { boxShadow: "0 0 0 0 rgba(226,56,46,0)" },
        },
      },
      animation: {
        "fade-up": "fade-up 700ms cubic-bezier(0.16,1,0.3,1) both",
        drift: "drift 6s ease-in-out infinite",
        "pulse-ring": "pulse-ring 2.2s cubic-bezier(0.4,0,0.6,1) infinite",
      },
    },
  },
  plugins: [],
};
