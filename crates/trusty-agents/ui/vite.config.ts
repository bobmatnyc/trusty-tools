import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Why: Tauri 2 hosts the built frontend from `ui/dist` and runs the dev
// server on a fixed port so the webview can reliably target it. Keep the port
// pinned and skip clearScreen so `pnpm tauri dev` shows both Vite and Rust
// logs in the same terminal, matching the ai-commander reference.
// What: Minimal Svelte 4 + Vite 5 config with Tauri-friendly build targets.
// Test: `pnpm build` produces `ui/dist/index.html` that Tauri can load.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // Why: In `pnpm dev` browser mode there is no Tauri runtime, so the UI
    // talks to `open-mpm --api` directly over HTTP. Without a proxy the
    // browser would issue cross-origin requests to a different port and run
    // into CORS — by proxying `/api/*` to the API server we keep all
    // requests same-origin in dev and avoid baking the API host into client
    // code. The port can be overridden via `VITE_OMPM_PORT` to match
    // whatever `--port` the API server was launched with.
    // What: Forward every `/api/*` request from Vite (5173) to
    // `http://localhost:<VITE_OMPM_PORT|7654>`.
    // Test: Run `cargo run -- --api --port 7654` in one shell, `pnpm dev` in
    // another, then `curl http://localhost:5173/api/health` and assert it
    // returns the API server's health JSON.
    proxy: {
      '/api': {
        target: `http://localhost:${process.env.VITE_OMPM_PORT ?? 7654}`,
        changeOrigin: true,
      },
    },
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari13'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    outDir: 'dist',
  },
});
