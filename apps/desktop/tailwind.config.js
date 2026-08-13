/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // The one saturated colour in the whole interface. Reserved for
        // recording: if it is red, audio is being captured.
        record: { DEFAULT: "#e2382e", hover: "#c92f26" },
        rail: "#fdfdfd",
        hairline: "#e9e9ec",
      },
      boxShadow: {
        dock: "0 4px 24px rgba(0,0,0,0.10), 0 1px 3px rgba(0,0,0,0.06)",
      },
    },
  },
  plugins: [],
};
