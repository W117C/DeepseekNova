import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";

export default defineConfig({
  plugins: [solid(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  resolve: {
    alias: {
      // opencode exports 语义：ui/* 默认落到 components/*，特殊子路径单独映射
      "@opencode-ai/ui/components": fileURLToPath(new URL("./vendor/ui/components", import.meta.url)),
      "@opencode-ai/ui/context": fileURLToPath(new URL("./vendor/ui/context", import.meta.url)),
      "@opencode-ai/ui/theme": fileURLToPath(new URL("./vendor/ui/theme", import.meta.url)),
      "@opencode-ai/ui/styles/index.css": fileURLToPath(new URL("./vendor/ui/styles/index.css", import.meta.url)),
      "@opencode-ai/ui/styles": fileURLToPath(new URL("./vendor/ui/styles", import.meta.url)),
      "@opencode-ai/ui/hooks": fileURLToPath(new URL("./vendor/ui/hooks", import.meta.url)),
      "@opencode-ai/ui/i18n": fileURLToPath(new URL("./vendor/ui/i18n", import.meta.url)),
      "@opencode-ai/ui/v2/styles": fileURLToPath(new URL("./vendor/ui/v2/styles", import.meta.url)),
      "@opencode-ai/ui/v2": fileURLToPath(new URL("./vendor/ui/v2/components", import.meta.url)),
      "@opencode-ai/ui": fileURLToPath(new URL("./vendor/ui/components", import.meta.url)),
      "@opencode-ai/session-ui": fileURLToPath(new URL("./vendor/session-ui", import.meta.url)),
      "@opencode-ai/sdk": fileURLToPath(new URL("./shims/sdk", import.meta.url)),
      "@opencode-ai/core": fileURLToPath(new URL("./shims/core", import.meta.url)),
      "@opencode-ai/client": fileURLToPath(new URL("./shims/client", import.meta.url)),
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    target: ["es2022", "chrome110", "safari16"],
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});