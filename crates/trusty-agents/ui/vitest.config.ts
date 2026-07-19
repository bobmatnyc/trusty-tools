import { defineConfig } from 'vitest/config';

// Why: Unit tests for framework-free pure functions (currently
// `src/lib/retask.ts`, #3063 code-critic follow-up) need a fast runner that
// doesn't require a live `trusty-agents --api` server or a browser — unlike
// `tests/*.spec.ts`, which are Playwright e2e specs run via `pnpm test`
// against a running server. Vitest pairs naturally with the existing Vite
// build and needs no extra config beyond scoping `include` so the two
// runners never pick up each other's files.
// What: Only collects `src/**/*.test.ts`; Playwright's `tests/` directory is
// untouched. No DOM/jsdom environment configured since the functions under
// test today are plain string/array logic with no Svelte component mounting.
export default defineConfig({
  test: {
    include: ['src/**/*.test.ts'],
  },
});
