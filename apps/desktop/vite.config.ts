import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  // Fixed port so the Tauri shell and the dev server agree on one URL.
  server: {
    port: 1420,
    strictPort: true,
    proxy: {
      // The engine binds to loopback only; proxying keeps the frontend
      // same-origin so no CORS handling is needed on the server side.
      "/v1": "http://127.0.0.1:47821",
      "/health": "http://127.0.0.1:47821",
    },
  },
  build: { outDir: "dist", emptyOutDir: true },
});
