import { mount } from 'svelte';
import App from './App.svelte';
import './lib/styles/tokens.css';

// Boot the Secrets page into #app (#9067).
const app = mount(App, { target: document.getElementById('app') });

export default app;
