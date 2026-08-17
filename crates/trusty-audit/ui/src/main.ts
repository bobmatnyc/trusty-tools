import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';

/**
 * Why: Svelte 5 components are functions, not classes — `new App(...)` throws
 * `component_api_invalid_new`. `mount` is the Svelte 5 replacement.
 * What: mounts the root component onto `#app` from `index.html`.
 * Test: `pnpm build` then launch the app; the window shows the guided status.
 */
const app = mount(App, {
  target: document.getElementById('app')!,
});

export default app;
