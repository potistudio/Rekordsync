import { defineConfig } from "vite";

// Tauri 向け設定: 固定ポートで起動し、src-tauri の変更は監視しない。
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/.venv/**", "**/src/**"] },
  },
  build: {
    outDir: "dist-web",
    emptyOutDir: true,
    target: "es2022",
  },
});
