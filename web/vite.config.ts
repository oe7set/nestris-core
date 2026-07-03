import { defineConfig } from "vite";

export default defineConfig({
  // The wasm package is a local file: dependency; keep it out of prebundling
  // so its init URL resolution works.
  optimizeDeps: { exclude: ["nestris-wasm"] },
  build: { target: "es2022" },
  worker: { format: "es" },
  server: {
    fs: {
      // The wasm pkg lives outside web/ (crates/nestris-wasm/pkg); allow the
      // dev server to serve from the workspace root, else it 403s the .wasm.
      allow: [".."],
    },
  },
});
