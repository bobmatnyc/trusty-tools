import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
// #5936: emptyOutDir below deletes the tracked ui-source-hash.txt; this
// re-writes it after the build that removed it.
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';

// Why: trusty-memory embeds the built dist/ directly in the Rust binary via
// rust-embed, so we want a self-contained, relative-path-friendly bundle.
// What: emit assets relative to the served root, target modern browsers
// (this is a developer-facing tool). During `vite dev` the API paths are
// proxied to the running daemon (default port 7079; override if your daemon
// binds elsewhere).
// Test: `pnpm build` produces ui/dist/index.html and ui/dist/assets/*.
export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-memory')],
  base: './',
  // Why: Svelte 5 exports map 'browser' → real client runtime and 'default' →
  // throwing SSR stub. Without pinning 'browser', Vite resolves to the SSR
  // stub and mount() throws "lifecycle_function_unavailable" at runtime.
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false
  },
  server: {
    port: 5174,
    proxy: {
      // Forward API + health calls to the daemon during dev.
      '/api': 'http://127.0.0.1:7079',
      '/health': 'http://127.0.0.1:7079'
    }
  }
});
