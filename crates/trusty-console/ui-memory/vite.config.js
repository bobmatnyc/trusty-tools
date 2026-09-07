import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
// #5936: emptyOutDir below deletes the tracked ui-source-hash.txt; this
// re-writes it after the build that removed it.
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';

// Why: the console embeds this bundle with rust-embed and serves it at
// `/tools/memory/`; the bytes must therefore be self-contained and
// relative-path-friendly, because the mount is a sub-path, not an origin root.
// What: emit assets relative to the served root, target modern browsers (this
// is a developer-facing tool).
// #6155: this project used to live at `crates/trusty-memory/ui` and build into
// `ui/dist/`, which trusty-memory embedded. trusty-memory no longer binds a
// listener (#6286, ADR-0032), so the source moved here and `outDir` writes
// straight into the crate-root `ui-memory-dist/` the console packages — no
// mirror step, and nothing left behind under `ui-memory/`.
// Test: `pnpm build` produces ../ui-memory-dist/index.html and
// ../ui-memory-dist/assets/*; `bash scripts/check-ui-bundle-freshness.sh
// trusty-console` then passes all three of the console's rows.
export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-console-memory')],
  base: './',
  // Why: Svelte 5 exports map 'browser' → real client runtime and 'default' →
  // throwing SSR stub. Without pinning 'browser', Vite resolves to the SSR
  // stub and mount() throws "lifecycle_function_unavailable" at runtime.
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: '../ui-memory-dist',
    // Required: outDir sits outside this Vite project root, so Vite refuses to
    // clear it unless asked explicitly.
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false
  },
  server: {
    // #6155: `vite dev` serves this SPA at the origin root, so base.js derives
    // `/` and these paths would hit the daemon directly — but trusty-memory has
    // no HTTP listener since #6286. They forward to the console's bridge
    // prefix instead, which is the only way in.
    port: 5174,
    proxy: {
      '/api/v1': 'http://127.0.0.1:7788/api/memory',
      '/health': 'http://127.0.0.1:7788/api/memory',
      '/sse': 'http://127.0.0.1:7788/api/memory'
    }
  }
});
