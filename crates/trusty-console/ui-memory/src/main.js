import { mount } from 'svelte';
import App from './App.svelte';
import './lib/styles/tokens.css';
import './lib/styles/global.css';
import { initThemeBootstrap } from './lib/theme-bootstrap.js';

// Why: Svelte 5 uses the `mount` API rather than `new App({ target })`.
// What: Boot the root component into #app after loading the shared design
// tokens + global resets/utility classes. Runs the Foundry theme bootstrap
// (issue #3487) first so `[data-theme='dark']` is set from OS appearance
// before the shell's first paintable content exists.
// Test: `pnpm build && pnpm preview` renders the dashboard with the dark
// sidebar and light content pane, matching OS appearance.
initThemeBootstrap();

const app = mount(App, { target: document.getElementById('app') });

export default app;
