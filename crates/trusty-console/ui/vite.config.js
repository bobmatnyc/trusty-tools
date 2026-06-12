import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  base: '/ui/',
  resolve: {
    // Ensure browser-side Svelte entry is resolved (not the server stub).
    // @sveltejs/vite-plugin-svelte adds 'svelte' to conditions, but Svelte 5
    // does not expose a 'svelte' condition on its root '.' export — only
    // 'browser' and 'default' (the latter points to index-server.js).
    // Without 'browser' here Vite falls through to 'default' and bundles the
    // SSR stubs, making onMount a no-op and mount() a throw.
    conditions: ['browser'],
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
});
