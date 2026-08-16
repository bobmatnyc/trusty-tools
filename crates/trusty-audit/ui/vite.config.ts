import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: Tauri 2 serves the built bundle from `ui/dist` and the dev webview
// targets a fixed port, so the port is pinned and `strictPort` makes a port
// clash an error rather than a silent move to 5174 that the webview cannot
// find. `clearScreen: false` keeps Vite and Rust logs in one terminal.
// What: Svelte 5 + Vite, Tauri-compatible build targets, output to `dist`.
// Test: `pnpm build` writes `ui/dist/index.html`, which `build.rs` requires.
//
// There is deliberately no `server.proxy`: this shell calls `Session::execute`
// in-process over Tauri IPC and never over HTTP, so there is no origin to
// proxy to (DOC-68 §11).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5183,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari13'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    outDir: 'dist',
  },
});
