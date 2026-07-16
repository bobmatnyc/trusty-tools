// Why: Tauri desktop entrypoint — mounts the Tauri-aware shell so the app
// renders inside the native window. Svelte 5 components are functions, not
// classes — `new App(...)` throws `component_api_invalid_new` at runtime;
// `mount` is the Svelte 5 replacement.
// What: Imports global styles and mounts App.svelte onto #app via `mount`.
// Test: `pnpm dev` then launch Tauri — the window shows the dashboard header.
import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';

const app = mount(App, {
  target: document.getElementById('app') as HTMLElement,
});

export default app;
