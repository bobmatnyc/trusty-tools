import './theme.css';
// #6518: the Foundry card/table/badge/stat-card subset the machine-status
// dashboard is built from. Loaded after theme.css, which owns the colours it
// references.
import './foundry.css';
import App from './App.svelte';
// #6519: the fullscreen screensaver, mounted instead of the console on its own
// route. This SPA has no router — App switches tabs by state — so the pathname
// is read once here, which is all a single alternate root needs.
import Screensaver from './Screensaver.svelte';
import { mount } from 'svelte';
import { initTheme } from './theme.svelte.js';
import { isScreensaverPath } from './screensaver.js';

// Still first: Screensaver forces dark on mount, but the theme store also owns
// the prefers-color-scheme listener, and leaving it uninitialised would strand
// the console on whatever the inline boot script in index.html set.
initTheme();

const root = isScreensaverPath(window.location.pathname) ? Screensaver : App;

const app = mount(root, { target: document.getElementById('app') });

export default app;
