// Why: Standalone browser entrypoint — mounts the REST-only shell so the same
// dashboard is reachable from a plain browser without the Tauri runtime.
// Svelte 5 components are functions, not classes — `new WebApp(...)` throws
// `component_api_invalid_new` at runtime; `mount` is the Svelte 5 replacement.
// What: Imports global styles and mounts WebApp.svelte onto #app via `mount`.
// Test: `pnpm build:web` then serve dist-web/ — the page loads and polls the
// daemon over REST.
import './app.css';
import { mount } from 'svelte';
import WebApp from './WebApp.svelte';

const app = mount(WebApp, {
  target: document.getElementById('app') as HTMLElement,
});

export default app;
