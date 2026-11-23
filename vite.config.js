import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { fileURLToPath } from "node:url";

const fixtureApi = fileURLToPath(new URL("./scripts/render/tauri-fixture-api.js", import.meta.url));

export default defineConfig(({ mode }) => ({
  plugins: [solid()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  resolve: {
    alias: mode === "fixture" ? {
      "@tauri-apps/api/core": fixtureApi,
      "@tauri-apps/api/event": fixtureApi,
    } : {},
  },
  build: { target: "safari15", outDir: mode === "fixture" ? "dist-fixture" : "dist" },
}));
