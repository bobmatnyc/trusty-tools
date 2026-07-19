// Why: Tauri desktop entrypoint — mounts the shell so it renders inside the
// native window. Svelte 5 components are functions, not classes —
// `new App(...)` throws `component_api_invalid_new`; `mount` is the Svelte 5
// replacement. Runs the Foundry theme bootstrap (`lib/theme-bootstrap.ts`,
// issue #3153 AC-27) before mounting so `[data-theme='dark']` is set from
// OS appearance before the shell's first paintable content exists.
// What: Imports global styles, applies the theme bootstrap, and mounts
// App.svelte onto #app via `mount`.
// Test: `pnpm dev` then launch Tauri — the window shows the health panel in
// the palette matching OS appearance; toggling System Settings > Appearance
// live-switches it without a reload.
import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';
import { initThemeBootstrap } from './lib/theme-bootstrap';

initThemeBootstrap();

const app = mount(App, {
  target: document.getElementById('app') as HTMLElement,
});

export default app;
