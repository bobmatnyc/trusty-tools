import { mount } from 'svelte';
import App from './App.svelte';
import './lib/styles/tokens.css';
// #11749: a verbatim copy of docs/design/UI/design-system/foundry.css.
import './lib/styles/foundry.css';

// Boot the Trusty Assistants page into #app (#11749).
const app = mount(App, { target: document.getElementById('app') });

export default app;
