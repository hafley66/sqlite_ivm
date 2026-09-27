import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { viteSingleFile } from "vite-plugin-singlefile";

export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  // code/1_map.ts reads the engine source from ../src with ?raw imports.
  server: { fs: { allow: [".."] } },
  test: { include: ["code/**/*.test.ts"] },
});
