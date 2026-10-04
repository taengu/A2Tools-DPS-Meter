import { defineConfig } from "vite";
import { copyFileSync, cpSync, mkdirSync } from "node:fs";
import { join } from "node:path";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({

  plugins: [{
    name: "ui-resources",
    configResolved(config) {
      const dataDir = join(config.publicDir, "data");
      mkdirSync(dataDir, { recursive: true });
      copyFileSync(join(config.root, "src/data/skill_icons.json"), join(dataDir, "skill_icons.json"));
      cpSync(join(config.root, "src/data/i18n"), join(config.publicDir, "i18n"), { recursive: true });
    },
  }],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
