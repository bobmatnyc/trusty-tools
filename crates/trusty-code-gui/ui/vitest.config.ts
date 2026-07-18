import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: `App.test.ts` pins DOC-39 §8.1's AC-18.1 DOM invariant (`.statusbar`
// is a sibling of `.body`, never a descendant) — the one piece of the
// visual system the spec calls a "testable invariant" after regressing
// twice in the wireframe. Kept as a separate config (not merged into
// `vite.config.ts`, which is the Tauri build entry) so `vitest` never
// touches the `frontendDist`/Tauri build settings.
// What: jsdom environment (component tests need a DOM to query) with the
// same `svelte()` plugin as the app build, so `.svelte` files compile
// identically in tests and in the real bundle.
// Test: `pnpm test` (see package.json).
export default defineConfig({
  plugins: [svelte()],
  // Without this, Vite's default SSR module resolution picks Svelte's
  // server-side `mount` (which throws `lifecycle_function_unavailable`)
  // even under `environment: 'jsdom'` — `resolve.conditions: ['browser']`
  // forces the client build, matching the real app bundle.
  resolve: {
    conditions: ['browser'],
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts'],
  },
});
