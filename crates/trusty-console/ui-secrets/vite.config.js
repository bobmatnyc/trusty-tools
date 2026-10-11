import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
// #5936: emptyOutDir below deletes the tracked ui-source-hash.txt; this
// re-writes it after the build that removed it.
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';

// Why: #9067 — the console embeds this bundle with rust-embed and serves it at
// `/tools/secrets/`, a sub-path, so every asset reference must be relative.
// What: emit relative assets into the crate-root `ui-secrets-dist/` and stamp
// it under the `trusty-console-secrets` manifest row.
// Test: `pnpm build` writes ../ui-secrets-dist/index.html and assets;
// `tests/secrets_ui_mount.rs` serves them through the real router.
export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-console-secrets')],
  base: './',
  // Svelte 5 maps 'browser' to the client runtime and 'default' to an SSR
  // stub; pin 'browser' or mount() throws at runtime (same as ui-analyze).
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: '../ui-secrets-dist',
    // outDir sits outside this Vite root, so Vite clears it only when asked.
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
    minify: true,
  },
  server: {
    // `vite dev` has no console in front of it. The secrets routes refuse any
    // Origin but the console's own, so there is no dev proxy: open the page
    // through the console at http://127.0.0.1:7788/tools/secrets/.
    port: 5176,
  },
  // `base.js` reads `document.baseURI` at module load and the page tests mount
  // components, so vitest needs a DOM.
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.js'],
  },
});
