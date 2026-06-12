import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: trusty-analyzer embeds the built dist/ directly in the Rust binary via
// include_dir!, so we want a self-contained, relative-path-friendly bundle.
// What: emit assets relative to the served root, target modern browsers
// (developer-facing tool), and proxy API + SSE paths to the analyzer daemon
// during `pnpm dev`.
// Test: `pnpm build` produces ui/dist/index.html and ui/dist/assets/*.
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
    minify: true,
  },
  server: {
    port: 5173,
    proxy: {
      '/health': 'http://127.0.0.1:7879',
      '/indexes': 'http://127.0.0.1:7879',
      '/facts': 'http://127.0.0.1:7879',
      '/sse': {
        target: 'http://127.0.0.1:7879',
        changeOrigin: true,
        ws: false,
      },
    },
  },
});
