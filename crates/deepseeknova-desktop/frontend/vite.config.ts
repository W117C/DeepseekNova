import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // Vite 8（rolldown + esbuild 转译）不再支持向 es2021/safari13 这类老目标
    // 降级解构语法；桌面端走系统 WebView，提升到现代目标。
    target: ["es2022", "chrome110", "safari16"],
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
