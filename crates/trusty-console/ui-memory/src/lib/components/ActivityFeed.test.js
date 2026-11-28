/*
 * Why: #10591 AC2 asks for a test of the Memory Activity panel's connected
 * state. The status label comes from the component's own `connected` state,
 * flipped by the `EventSource` `onopen` and `onerror` handlers, and nothing
 * else in the suite asserts it.
 * What: Mounts the real ActivityFeed in jsdom with a fake `EventSource` and a
 * stubbed `fetch` (the API boundary only), then checks the header label reads
 * `reconnecting…` before the stream opens, `live` after `onopen`, and
 * `reconnecting…` again after `onerror`. Covers AC2 only; AC1 needs reporter data.
 * Test: this file.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

import ActivityFeed from './ActivityFeed.svelte';

/** Minimal EventSource double: records instances, lets the test fire handlers. */
class FakeEventSource {
  static instances = [];
  constructor(url) {
    this.url = url;
    this.closed = false;
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    FakeEventSource.instances.push(this);
  }
  close() {
    this.closed = true;
  }
}

let target = null;
let instance = null;

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  // History and palace-name fetches resolve empty; only the stream is under test.
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify({ entries: [], total: 0 }), {
      status: 200,
      headers: { 'content-type': 'application/json' }
    }))
  );
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  if (target) target.remove();
  target = null;
  vi.unstubAllGlobals();
});

function render() {
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(ActivityFeed, { target });
  flushSync();
  return target;
}

const statusText = () => target.querySelector('.status-text').textContent.trim();

describe('ActivityFeed connection status', () => {
  it('shows live after onopen and reconnecting after onerror', () => {
    render();
    expect(FakeEventSource.instances).toHaveLength(1);
    const es = FakeEventSource.instances[0];
    expect(es.url).toContain('/sse');

    expect(statusText()).toBe('reconnecting…');
    expect(target.querySelector('.dot').classList.contains('offline')).toBe(true);

    es.onopen();
    flushSync();
    expect(statusText()).toBe('live');
    expect(target.querySelector('.dot').classList.contains('online')).toBe(true);

    es.onerror();
    flushSync();
    expect(statusText()).toBe('reconnecting…');
    expect(target.querySelector('.dot').classList.contains('offline')).toBe(true);
    expect(es.closed).toBe(true);
  });
});
