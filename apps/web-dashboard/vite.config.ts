import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    // 1430, clear of the desktop frontend on 1420 — both are often up at once.
    port: 1430,
    strictPort: true,
    proxy: {
      // The engine is loopback-only. Proxying keeps this frontend same-origin, so there is
      // no CORS to configure on a server that deliberately has none.
      "/v1": "http://127.0.0.1:47821",
      "/health": "http://127.0.0.1:47821",
    },
  },
  build: { outDir: "dist", emptyOutDir: true },
});
