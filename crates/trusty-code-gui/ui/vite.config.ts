import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Tauri build — index.html entry, output to ./dist which tauri.conf.json
// references as `frontendDist`. Port 5174 (mpm-gui uses 5173) so both
// desktop shells can run `pnpm dev` side by side.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari13'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
