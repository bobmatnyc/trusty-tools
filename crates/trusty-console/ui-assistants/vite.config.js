import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
// #5936: emptyOutDir below deletes the tracked ui-source-hash.txt; this
// re-writes it after the build that removed it.
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';

// Why: #11749 — the console embeds this bundle with rust-embed and serves it
// at `/tools/assistants/`, a sub-path, so every asset reference must be relative.
// What: emit relative assets into the crate-root `ui-assistants-dist/` and
// stamp it under the `trusty-console-assistants` manifest row.
// Test: `pnpm build` writes ../ui-assistants-dist/index.html and assets;
// `tests/assistants_ui_mount.rs` serves them through the real router.
export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-console-assistants')],
  base: './',
  // Svelte 5 maps 'browser' to the client runtime and 'default' to an SSR
  // stub; pin 'browser' or mount() throws at runtime (same as ui-secrets).
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: '../ui-assistants-dist',
    // outDir sits outside this Vite root, so Vite clears it only when asked.
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
    minify: true,
  },
  server: {
    // `vite dev` has no console in front of it, and the ask route refuses
    // any Origin but the console's own, so there is no dev proxy: open the
    // page through the console at http://127.0.0.1:7788/tools/assistants/.
    port: 5177,
  },
  // `base.js` reads `document.baseURI` at module load and the page tests mount
  // components, so vitest needs a DOM.
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.js'],
  },
});
