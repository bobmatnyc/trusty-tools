/*
 * Why: #7589 put the Foundry brand lockup in this dashboard's header. A lockup
 * that is documented but not actually mounted is the regression this file
 * exists to catch — the Topbar is the one place that answers "which product is
 * this", and nothing else in the suite asserts it. The assertion is structural
 * rather than visual because a screenshot cannot run in CI.
 * What: Mounts the real Topbar in jsdom and asserts `ToolLockup` renders the
 * canonical Foundry robot mark and this tool's wordmark inside the header, in
 * the lead cluster ahead of the breadcrumbs.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

// `state.svelte.js` calls `applyTheme()` at MODULE scope, which reaches for
// `window.matchMedia` — absent in jsdom. `vi.hoisted` runs before the hoisted
// import below, the only point early enough to install a stub.
vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    writable: true,
    value: (query) => ({
      matches: false,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      onchange: null,
      dispatchEvent: () => false
    })
  });
});

import Topbar from './Topbar.svelte';

let target = null;
let instance = null;

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  if (target) target.remove();
  target = null;
});

function render() {
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(Topbar, { target });
  flushSync();
  return target;
}

/** First class name of an element, with Svelte's scoping suffix dropped. */
function baseClass(el) {
  return el.className.split(' ')[0];
}

describe('Topbar brand lockup', () => {
  it('mounts the Foundry lockup inside the header', () => {
    const header = render().querySelector('header.topbar');
    expect(header).not.toBeNull();

    const lockup = header.querySelector('.tool-lockup');
    expect(lockup).not.toBeNull();
    // RobotIcon labels itself, which is how this proves the mark beside the
    // wordmark is the canonical Foundry one and not an ad hoc glyph.
    expect(lockup.querySelector('svg[aria-label="Trusty Assistant robot"]')).not.toBeNull();
    expect(lockup.querySelector('.tool-lockup-name').textContent).toBe('Trusty Analyzer');
  });

  it('places the lockup ahead of the breadcrumbs in one lead cluster', () => {
    const lead = render().querySelector('header.topbar .lead');
    expect(lead).not.toBeNull();
    expect([...lead.children].map(baseClass)).toEqual([
      'tool-lockup',
      'lead-divider',
      'crumbs'
    ]);
  });
});
