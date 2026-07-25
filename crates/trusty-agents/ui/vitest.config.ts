import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: Unit tests for framework-free pure functions (`src/lib/*.ts`,
// `src/stores/*.ts`) need a fast runner that doesn't require a live
// `trusty-agents --api` server or a browser — unlike `tests/*.spec.ts`, which
// are Playwright e2e specs run via `pnpm test` against a running server.
// Vitest pairs naturally with the existing Vite build and needs no extra
// config beyond scoping `include` so the two runners never pick up each
// other's files. #3894 additionally mounts real components: the config
// takeover's "chat is covered, never unmounted" invariant is a DOM-structure
// property no pure function can express. That needs a DOM plus the same
// `svelte()` plugin the app build uses, so `.svelte` files compile
// identically in tests and in the real bundle — mirroring
// `crates/trusty-code-gui/ui/vitest.config.ts`, which already does this.
// What: jsdom environment, svelte plugin, browser resolve conditions; still
// only collects `src/**/*.test.ts`, leaving Playwright's `tests/` untouched.
// Kept separate from `vite.config.ts` (the Tauri build entry) so vitest never
// touches the app's build/proxy settings.
export default defineConfig({
  plugins: [svelte()],
  // Without this, Vite's default SSR module resolution picks Svelte's
  // server-side `mount` (which throws `lifecycle_function_unavailable`) even
  // under `environment: 'jsdom'` — `resolve.conditions: ['browser']` forces
  // the client build, matching the real app bundle.
  resolve: {
    conditions: ['browser'],
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts'],
  },
});
