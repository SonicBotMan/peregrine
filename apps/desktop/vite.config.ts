import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Dev server proxies to the daemon's loopback TCP (start it with
// `peregrined --listen tcp:8420` or the dual `tcp:8420+unix:…`).
// Production (Tauri) bundles the built assets; the webview's fetch
// base is wired in M3-b.
export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5199,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8420',
        rewrite: (p) => p.replace(/^\/api/, ''),
      },
      '/ws': {
        target: 'ws://127.0.0.1:8420',
        ws: true,
        rewrite: (p) => p.replace(/^\/ws/, ''),
      },
    },
  },
});
