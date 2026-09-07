import { mount } from 'svelte';
import App from './App.svelte';
import './lib/styles/tokens.css';
import './lib/styles/global.css';

// Why: Svelte 5 uses the `mount` API rather than `new App({ target })`.
// What: Boot the root component into #app after loading the shared design
// tokens + global resets/utility classes.
// Test: `pnpm build && pnpm preview` renders the dashboard with the dark
// sidebar and light content pane.
const app = mount(App, { target: document.getElementById('app') });

export default app;
