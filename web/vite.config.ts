import { defineConfig } from "vite";

export default defineConfig({
  // The wasm package is a local file: dependency; keep it out of prebundling
  // so its init URL resolution works.
  optimizeDeps: { exclude: ["nestris-wasm"] },
  build: { target: "es2022" },
  worker: { format: "es" },
});
