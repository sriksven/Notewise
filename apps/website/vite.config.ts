import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  // GitHub Pages serves this repo at /Notewise/, so every asset URL needs the prefix.
  // A custom domain would set this back to "/".
  base: "/Notewise/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      // Two entry points: the pitch, and the page that tells you how to run it.
      input: {
        main: resolve(__dirname, "index.html"),
        download: resolve(__dirname, "download/index.html"),
      },
    },
  },
  server: { port: 1440, strictPort: true },
});
