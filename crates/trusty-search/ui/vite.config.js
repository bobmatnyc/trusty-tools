import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: trusty-search embeds the built dist/ directly in the Rust binary via
// include_dir!, so we want a self-contained, relative-path-friendly bundle.
// What: emit assets relative to the served root, do not split chunks
// excessively, target modern browsers (since this is a developer-facing tool).
// Test: `npm run build` produces ui/dist/index.html and ui/dist/assets/*.
export default defineConfig({
  plugins: [svelte()],
  base: './',
  // Why: Svelte 5 exports map 'browser' → real client runtime and 'default' →
  // throwing SSR stub. Without pinning 'browser', Vite resolves to the SSR
  // stub and mount() throws "lifecycle_function_unavailable" at runtime.
  resolve: {
    conditions: ['browser', 'module', 'import', 'default'],
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
  },
  server: {
    port: 5173,
    proxy: {
      // Forward API calls to the daemon during dev.
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
});
