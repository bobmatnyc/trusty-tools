import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: Svelte 5 ships separate browser and server export conditions in its
// package.json exports map. Without pinning the 'browser' condition, Vite
// resolves to the 'default' (server/SSR) stub, which throws
// "lifecycle_function_unavailable: mount(...) is not available on the server"
// at runtime. Pinning 'browser' first forces resolution to the correct
// client-side runtime that implements mount().
// What: Adds resolve.conditions so Vite always picks the browser build of
// Svelte 5 when bundling for the console SPA.
// Test: Build with `pnpm run build`; the resulting bundle must not contain
// the SSR stub and must mount without throwing in a browser.
export default defineConfig({
  plugins: [svelte()],
  base: '/ui/',
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
});
