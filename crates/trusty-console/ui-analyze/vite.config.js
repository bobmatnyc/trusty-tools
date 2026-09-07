import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
// #5936: emptyOutDir below deletes the tracked ui-source-hash.txt; this
// re-writes it after the build that removed it.
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';

// Why: the console embeds this bundle with rust-embed and serves it at
// `/tools/analyze/`; the bytes must therefore be self-contained and
// relative-path-friendly, because the mount is a sub-path, not an origin root.
// What: emit assets relative to the served root, target modern browsers (this
// is a developer-facing tool).
// #6155: this project used to live at `crates/trusty-analyze/ui` and build into
// `ui/dist/`, which trusty-analyze embedded. trusty-analyze no longer binds a
// listener (#6287, ADR-0032), so the source moved here and `outDir` writes
// straight into the crate-root `ui-analyze-dist/` the console packages — no
// mirror step, and nothing left behind under `ui-analyze/`.
// Test: `pnpm build` produces ../ui-analyze-dist/index.html and
// ../ui-analyze-dist/assets/*; `bash scripts/check-ui-bundle-freshness.sh
// trusty-console` then passes all four of the console's rows.
export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-console-analyze')],
  base: './',
  // Why: Svelte 5 exports map 'browser' → real client runtime and 'default' →
  // throwing SSR stub. Without pinning 'browser', Vite resolves to the SSR
  // stub and mount() throws "lifecycle_function_unavailable" at runtime.
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: '../ui-analyze-dist',
    // Required: outDir sits outside this Vite project root, so Vite refuses to
    // clear it unless asked explicitly.
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
    minify: true,
  },
  server: {
    // #6155: `vite dev` serves this SPA at the origin root, so base.js derives
    // `/` and these paths would hit the daemon directly — but trusty-analyze
    // has no HTTP listener since #6287. They forward to the console's bridge
    // prefix instead, which is the only way in.
    port: 5175,
    proxy: {
      '/health': 'http://127.0.0.1:7788/api/analyze',
      '/indexes': 'http://127.0.0.1:7788/api/analyze',
      '/facts': 'http://127.0.0.1:7788/api/analyze'
    }
  }
});
