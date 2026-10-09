import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';
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
// Why: the hit viewer imports the shared Foundry code/diff components from
// their canonical home rather than a copy (`@foundry`). They sit outside this
// package, so the dev server must be allowed to read them, and `svelte` must
// resolve from this package's node_modules for them as well (`dedupe`). This
// bundle ships prebuilt, so the cross-tree import never reaches a crate tarball.
// What: the `@foundry` alias, `server.fs.allow`, and `resolve.dedupe`.
// Test: `pnpm build` succeeds and `HitViewer.test.js` mounts the shared view.
const FOUNDRY_COMPONENTS = fileURLToPath(
  new URL('../../../docs/design/UI/design-system/components', import.meta.url),
);

export default defineConfig({
  plugins: [svelte(), stampUiBundle('trusty-console-search')],
  base: './',
  // Why: Svelte 5 exports map 'browser' → real client runtime and 'default' →
  // throwing SSR stub. Without pinning 'browser', Vite resolves to the SSR
  // stub and mount() throws "lifecycle_function_unavailable" at runtime.
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
    alias: { '@foundry': FOUNDRY_COMPONENTS },
    dedupe: ['svelte'],
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
    fs: { allow: ['.', FOUNDRY_COMPONENTS] },
    proxy: {
      // Forward API calls through the console's `/api/search` bridge, which
      // reaches trusty-search over its Unix socket (#9214); the daemon has no
      // TCP listener to dial. `vite dev` serves the SPA at the origin root, so
      // base.js derives `/` and `/indexes` lands on `/api/search/indexes`.
      // Not proxied: `/facts` (no api.js caller, no bridge row) and `/admin`
      // (`POST /admin/stop` answers 501 from search_uds/map.rs).
      '/health': 'http://127.0.0.1:7788/api/search',
      '/status': 'http://127.0.0.1:7788/api/search',
      '/indexes': 'http://127.0.0.1:7788/api/search',
      '/search': 'http://127.0.0.1:7788/api/search',
      '/chat': 'http://127.0.0.1:7788/api/search',
      '/logs': 'http://127.0.0.1:7788/api/search',
      '/config': 'http://127.0.0.1:7788/api/search',
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
