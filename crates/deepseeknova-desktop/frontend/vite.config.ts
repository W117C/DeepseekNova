import { defineConfig } from 'vitest/config';
import solid from 'vite-plugin-solid';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  plugins: [solid(), tailwindcss()],
  server: { port: 5173 },
  preview: { port: 4173 },
  build: { outDir: 'dist', target: 'es2022' },
  test: { environment: 'node' },
});
