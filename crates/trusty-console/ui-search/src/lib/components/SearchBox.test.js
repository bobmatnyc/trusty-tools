/*
 * Why: the search box's typeahead is timing-sensitive — a debounce that does
 * not hold, or a slow answer for an old prefix that overwrites the current
 * list, are both invisible until an operator hits them.
 * What: mounts the real SearchBox with only the API module mocked, drives it
 * with fake timers and real DOM events, and checks the debounce, the ARIA
 * combobox keyboard contract, the stale-response drop, and the 501 shut-off.
 * Test: this file.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

vi.mock('../api.js', () => ({ api: { typeahead: vi.fn() } }));

import { api } from '../api.js';
import SearchBox from './SearchBox.svelte';

let target = null;
let instance = null;

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  target?.remove();
  target = null;
  vi.useRealTimers();
  vi.clearAllMocks();
});

function render(props = {}) {
  target = document.createElement('div');
  document.body.appendChild(target);
  const onsearch = vi.fn();
  instance = mount(SearchBox, { target, props: { indexIds: ['ix'], onsearch, ...props } });
  flushSync();
  const input = target.querySelector('input[role="combobox"]');
  input.focus();
  return { input, onsearch };
}

function type(input, value) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}

function key(input, k) {
  input.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }));
  flushSync();
}

const hit = (label, score = 1) => ({ label, path: `src/${label}.rs`, start_line: 1, score, source: 'lexical', snippet: '' });

async function settle() {
  for (let i = 0; i < 10; i++) await Promise.resolve();
  flushSync();
}

function options() {
  return [...target.querySelectorAll('[role="option"]')];
}

describe('SearchBox typeahead', () => {
  it('sends one request per pause, 150 ms after the last keystroke', async () => {
    api.typeahead.mockResolvedValue({ hits: [] });
    const { input } = render();
    type(input, 'a');
    type(input, 'au');
    type(input, 'aut');
    vi.advanceTimersByTime(149);
    expect(api.typeahead).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.typeahead).toHaveBeenCalledTimes(1);
    expect(api.typeahead.mock.calls[0].slice(0, 2)).toEqual(['ix', 'aut']);
  });

  it('moves through options with the arrow keys, runs the chosen one on Enter, and closes on Escape', async () => {
    api.typeahead.mockResolvedValue({ hits: [hit('alpha', 3), hit('beta', 2)] });
    const { input, onsearch } = render();
    type(input, 'a');
    vi.advanceTimersByTime(150);
    await settle();
    expect(input.getAttribute('aria-expanded')).toBe('true');
    expect(options().map((o) => o.querySelector('.label').textContent)).toEqual(['alpha', 'beta']);

    key(input, 'ArrowDown');
    expect(input.getAttribute('aria-activedescendant')).toBe(options()[0].id);
    expect(options()[0].getAttribute('aria-selected')).toBe('true');
    key(input, 'ArrowDown');
    key(input, 'ArrowDown'); // wraps to the first option
    expect(input.getAttribute('aria-activedescendant')).toBe(options()[0].id);
    key(input, 'ArrowUp'); // wraps to the last option
    expect(input.getAttribute('aria-activedescendant')).toBe(options()[1].id);

    key(input, 'Escape');
    expect(input.getAttribute('aria-expanded')).toBe('false');
    expect(input.hasAttribute('aria-activedescendant')).toBe(false);

    key(input, 'ArrowDown'); // reopens on the first option
    key(input, 'Enter');
    expect(onsearch).toHaveBeenCalledWith('alpha');
    expect(input.value).toBe('alpha');
    expect(input.getAttribute('aria-expanded')).toBe('false');
  });

  it('drops a slow answer for an older prefix and aborts its request', async () => {
    const pending = [];
    api.typeahead.mockImplementation(
      (_id, q, _limit, signal) => new Promise((resolve) => pending.push({ q, signal, resolve }))
    );
    const { input } = render();
    type(input, 'ab');
    vi.advanceTimersByTime(150);
    type(input, 'abc');
    vi.advanceTimersByTime(150);
    expect(pending.map((p) => p.q)).toEqual(['ab', 'abc']);
    expect(pending[0].signal.aborted).toBe(true);

    pending[1].resolve({ hits: [hit('current')] });
    await settle();
    pending[0].resolve({ hits: [hit('stale')] });
    await settle();
    expect(options().map((o) => o.querySelector('.label').textContent)).toEqual(['current']);
  });

  it('stops asking after the server answers 501 (no typeahead route)', async () => {
    api.typeahead.mockRejectedValue(Object.assign(new Error('501'), { status: 501 }));
    const { input } = render();
    type(input, 'a');
    vi.advanceTimersByTime(150);
    await settle();
    type(input, 'ab');
    vi.advanceTimersByTime(150);
    expect(api.typeahead).toHaveBeenCalledTimes(1);
    expect(options()).toHaveLength(0);
  });
});
