import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
// #5936: emptyOutDir below deletes the tracked ui-source-hash.txt; this
// re-writes it after the build that removed it.
import { stampUiBundle } from '../../../scripts/lib/vite-stamp-bundle.mjs';

// Why: the console embeds this bundle with rust-embed and serves it at
// `/tools/search/`; the bytes must therefore be self-contained and
// relative-path-friendly, because the mount is a sub-path, not an origin root.
// What: emit assets relative to the served root, do not split chunks
// excessively, target modern browsers (this is a developer-facing tool).
// #6155: `outDir` writes straight into the crate-root `ui-search-dist/` the
// console packages, so `pnpm build`, `make -C crates/trusty-console search-ui`,
// and `cargo build -p trusty-console` all produce the one committed bundle
// rather than a `dist/` that then has to be mirrored.
// Test: `pnpm build` produces ../ui-search-dist/index.html and
// ../ui-search-dist/assets/*; `bash scripts/check-ui-bundle-freshness.sh
// trusty-console` then passes both of the console's rows.
export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-console-search')],
  base: './',
  // Why: Svelte 5 exports map 'browser' → real client runtime and 'default' →
  // throwing SSR stub. Without pinning 'browser', Vite resolves to the SSR
  // stub and mount() throws "lifecycle_function_unavailable" at runtime.
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: '../ui-search-dist',
    // Required: outDir sits outside this Vite project root, so Vite refuses to
    // clear it unless asked explicitly.
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
  },
  server: {
    port: 5173,
    proxy: {
      // Forward API calls to the trusty-search daemon during dev. The console
      // is not in the loop here — `vite dev` serves the SPA at the origin root,
      // so base.js derives `/` and these paths hit the daemon directly.
      // #6285 retires that listener; this block moves to the console's
      // `/api/search/` prefix with it.
      '/health': 'http://127.0.0.1:7878',
      '/status': 'http://127.0.0.1:7878',
      '/indexes': 'http://127.0.0.1:7878',
      '/search': 'http://127.0.0.1:7878',
      '/chat': 'http://127.0.0.1:7878',
      '/facts': 'http://127.0.0.1:7878',
      '/logs': 'http://127.0.0.1:7878',
      '/config': 'http://127.0.0.1:7878',
      '/admin': 'http://127.0.0.1:7878',
    },
  },
  // Why: the API base-URL derivation (src/lib/base.js) reads document.baseURI,
  // so its regression tests (issue #1329) need a DOM. jsdom gives vitest a
  // `document`/`window` to stub. Test: `pnpm test` runs src/lib/base.test.js.
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.js'],
  },
});
