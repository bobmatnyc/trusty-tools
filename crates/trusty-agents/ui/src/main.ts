import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';

/**
 * Why: Svelte 5 components are plain functions, not classes — `new App(...)`
 * throws `component_api_invalid_new` at runtime (dev) or silently mis-invokes
 * the function (prod). The `mount` API is the Svelte 5 replacement.
 * What: Mount the root App component against the `#app` div from
 * `index.html`.
 * Test: `pnpm build && pnpm preview` then load the page and confirm `#app`
 * has rendered children (see also the smoke check run against `dist/`).
 */
const app = mount(App, {
  target: document.getElementById('app')!,
});

export default app;
