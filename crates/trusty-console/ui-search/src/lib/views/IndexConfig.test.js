/*
 * Why: #9059 closes only when the console shows a held index. The settings page
 * is where the fix happens, so it must show the hold, the glob, and the fix.
 * What: mounts the real IndexConfig view with the API module mocked and checks
 * the held and not-held renders, and the message when a save releases a hold.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

vi.mock('../api.js', () => ({
  api: { getIndexConfig: vi.fn(), updateIndexConfig: vi.fn(), reindex: vi.fn() }
}));

import { api } from '../api.js';
import IndexConfig from './IndexConfig.svelte';

/** A `GET /indexes/{id}/config` body (`IndexConfigView`). */
function configBody(overrides = {}) {
  return {
    extra_skip_dirs: [],
    data_file_max_bytes: 65536,
    extensions: [],
    exclude_globs: ['**/.env', '**.min.js'],
    invalid_exclude_globs: [],
    include_docs: true,
    respect_gitignore: true,
    ...overrides
  };
}

let target = null;
let instance = null;

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  target?.remove();
  target = null;
  vi.clearAllMocks();
});

async function render(config) {
  api.getIndexConfig.mockResolvedValue(config);
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(IndexConfig, { target, props: { id: 'proj' } });
  await vi.waitFor(() => {
    flushSync();
    expect(target.querySelector('.card-header')).not.toBeNull();
  });
  return target;
}

describe('IndexConfig held-index banner', () => {
  it('shows the held index, its invalid glob, and the fix', async () => {
    const el = await render(configBody({ invalid_exclude_globs: ['**.min.js'] }));
    const banner = el.querySelector('[data-testid="held-index-banner"]');
    expect(banner).not.toBeNull();
    expect(banner.getAttribute('role')).toBe('alert');
    expect(banner.textContent).toContain('Index proj is held');
    expect([...banner.querySelectorAll('[data-testid="held-index-globs"] code')].map((c) => c.textContent)).toEqual([
      '**.min.js'
    ]);
    expect(banner.querySelector('[data-testid="held-index-hint"]').textContent).toContain(
      'releases the hold and starts a catch-up reindex'
    );
  });

  it('shows no banner when every glob parses', async () => {
    const el = await render(configBody());
    expect(el.querySelector('[data-testid="held-index-banner"]')).toBeNull();
  });

  it('drops the banner and reports the catch-up once a save releases the hold', async () => {
    const el = await render(configBody({ invalid_exclude_globs: ['**.min.js'] }));
    api.updateIndexConfig.mockResolvedValue({
      id: 'proj',
      config: configBody({ exclude_globs: ['**/.env'] }),
      reindex_required: true,
      catch_up_reindex: { started: true, stream_url: '/indexes/proj/reindex/stream' }
    });
    // Remove the invalid glob through the tag list, then save.
    const chips = [...el.querySelectorAll('button')].filter((b) =>
      (b.getAttribute('aria-label') ?? '').includes('**.min.js')
    );
    expect(chips).toHaveLength(1);
    chips[0].click();
    flushSync();
    [...el.querySelectorAll('button')].find((b) => b.textContent.includes('Save changes')).click();
    await vi.waitFor(() => {
      flushSync();
      expect(el.textContent).toContain('catch-up reindex started');
    });
    expect(api.updateIndexConfig).toHaveBeenCalledWith('proj', { exclude_globs: ['**/.env'] });
    expect(el.querySelector('[data-testid="held-index-banner"]')).toBeNull();
    // The catch-up already runs, so no second "Reindex now" prompt.
    expect(el.textContent).not.toContain('Reindex required');
  });
});
