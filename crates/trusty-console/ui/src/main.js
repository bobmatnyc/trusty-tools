import './theme.css';
// #6518: the Foundry card/table/badge/stat-card subset the machine-status
// dashboard is built from. Loaded after theme.css, which owns the colours it
// references.
import './foundry.css';
import App from './App.svelte';
import { mount } from 'svelte';
import { initTheme } from './theme.svelte.js';

initTheme();

const app = mount(App, { target: document.getElementById('app') });

export default app;
