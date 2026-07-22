import { defineConfig } from 'vite';

// Why: Introduced alongside #3315 (removal of the dead `daemonUrl`
// localStorage override) so `apiBase()`'s remaining behavior — and any
// future lib-level unit test in this crate — has somewhere to run. Kept as
// its own config (not merged into `vite.config.ts`, the Tauri build entry)
// so `vitest` never touches the `frontendDist`/Tauri build settings.
// What: Plain Node environment — `api-config.ts` no longer touches
// `window`/`localStorage` at all, so no DOM environment (jsdom/happy-dom) is
// needed for the tests this config currently runs.
// Test: `pnpm test` (see package.json).
export default defineConfig({
  test: {
    include: ['src/**/*.test.ts'],
  },
});
