/// <reference types="vitest" />
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

/**
 * The frontend is served either by the LayerAgent Rust web server (production)
 * or by Vite in development. In development the `/api` prefix is proxied to the
 * local daemon so that the browser keeps using same-origin relative URLs.
 */
const apiTarget = process.env.LAYERAGENT_API_TARGET ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: apiTarget,
        changeOrigin: false,
        ws: false,
      },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: false,
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/__tests__/setup.ts'],
    css: false,
    include: ['src/**/*.test.{ts,tsx}'],
  },
});
