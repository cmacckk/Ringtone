import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 约定：固定端口，忽略 src-tauri 目录的热更新
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "chrome110",
    sourcemap: false,
  },
});
