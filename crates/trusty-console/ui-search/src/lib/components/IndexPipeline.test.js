/*
 * Why: the Indexes roster's expanded row is where an operator triages an index,
 * and a held index (#9059) must not read "Healthy" there.
 * What: mounts the real IndexPipeline with the API module mocked and checks a
 * held status body shows the banner with the daemon's reason and a settings
 * link, and a ready one shows none.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

vi.mock('../api.js', () => ({
  api: { indexStatus: vi.fn(), pauseEmbedding: vi.fn(), resumeEmbedding: vi.fn() },
  fileEventsStreamUrl: (id) => `/indexes/${id}/file-events/stream`
}));

import { api } from '../api.js';
import IndexPipeline from './IndexPipeline.svelte';

const READY_STAGES = {
  lexical: { status: 'ready' },
  graph: { status: 'ready' },
  semantic: { status: 'ready' }
};
const HOLD_REASON =
  'index \'proj\' is held: exclude glob(s) ["**.min.js"] do not parse, so the paths they ' +
  'name cannot be excluded (#9059)';

let target = null;
let instance = null;

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  target?.remove();
  target = null;
  vi.clearAllMocks();
});

async function render(status) {
  api.indexStatus.mockResolvedValue(status);
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(IndexPipeline, { target, props: { id: 'proj' } });
  await vi.waitFor(() => {
    flushSync();
    expect(target.querySelector('[data-testid="index-health"]').textContent).not.toContain('Unknown');
  });
  return target;
}

describe('IndexPipeline held-index banner', () => {
  it('shows the daemon reason and links to the index settings', async () => {
    const el = await render({ status: 'held', last_walk_error: HOLD_REASON, stages: READY_STAGES });
    const banner = el.querySelector('[data-testid="held-index-banner"]');
    expect(banner).not.toBeNull();
    expect(banner.textContent).toContain('Index proj is held');
    expect(banner.querySelector('[data-testid="held-index-reason"]').textContent).toBe(HOLD_REASON);
    expect(banner.querySelector('a').getAttribute('href')).toBe('#/indexes/proj/config');
    expect(el.querySelector('[data-testid="index-health"]').textContent).toContain('Held');
  });

  it('shows no banner for a ready index', async () => {
    const el = await render({ status: 'ready', last_walk_error: null, stages: READY_STAGES });
    expect(el.querySelector('[data-testid="held-index-banner"]')).toBeNull();
    expect(el.querySelector('[data-testid="index-health"]').textContent).not.toContain('Held');
  });
});
