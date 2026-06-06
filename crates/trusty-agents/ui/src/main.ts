import './app.css';
import App from './App.svelte';

/**
 * Why: Svelte 4 is mounted imperatively at the `#app` root. Keeping this file
 * tiny keeps the hot-reload cycle fast and mirrors the ai-commander reference
 * so future Svelte 5 migrations touch only this file.
 * What: Construct the root App component against the `#app` div from
 * `index.html`.
 * Test: `pnpm build && grep -l "#app" dist/assets/*.js` shows the bundled app
 * references the mount target.
 */
const app = new App({
  target: document.getElementById('app')!,
});

export default app;
