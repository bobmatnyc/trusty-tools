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

  /** Toggle "Include documentation files" so the form is dirty, then click Save. */
  async function saveDocsToggle(el, expectedText) {
    const box = el.querySelector('input[type="checkbox"]');
    box.click();
    flushSync();
    [...el.querySelectorAll('button')].find((b) => b.textContent.includes('Save changes')).click();
    await vi.waitFor(() => {
      flushSync();
      expect(el.textContent).toContain(expectedText);
    });
  }

  const reindexNowButton = (el) => [...el.querySelectorAll('button')].find((b) => b.textContent.includes('Reindex now'));

  it('offers no "Reindex now" while the saved config is still held', async () => {
    const el = await render(configBody());
    api.updateIndexConfig.mockResolvedValue({
      id: 'proj',
      config: configBody({ include_docs: false, invalid_exclude_globs: ['**.min.js'] }),
      reindex_required: true
    });
    await saveDocsToggle(el, 'Settings saved.');
    expect(el.querySelector('[data-testid="held-index-banner"]')).not.toBeNull();
    expect(reindexNowButton(el)).toBeUndefined();
    expect(el.textContent).not.toContain('Reindex required');
  });

  it('re-reads the config and shows the banner when Reindex now answers 409', async () => {
    const el = await render(configBody());
    api.updateIndexConfig.mockResolvedValue({
      id: 'proj',
      config: configBody({ include_docs: false }),
      reindex_required: true
    });
    await saveDocsToggle(el, 'Reindex required');
    expect(el.querySelector('[data-testid="held-index-banner"]')).toBeNull();
    api.reindex.mockRejectedValue(Object.assign(new Error('index is held'), { status: 409 }));
    api.getIndexConfig.mockClear();
    api.getIndexConfig.mockResolvedValue(configBody({ invalid_exclude_globs: ['**.min.js'] }));
    reindexNowButton(el).click();
    await vi.waitFor(() => {
      flushSync();
      expect(el.querySelector('[data-testid="held-index-banner"]')).not.toBeNull();
    });
    expect(api.getIndexConfig).toHaveBeenCalledTimes(1);
    expect(el.textContent).toContain('Reindex failed: index is held');
  });

  it('shows the reason when the catch-up reindex did not start', async () => {
    const el = await render(configBody({ invalid_exclude_globs: ['**.min.js'] }));
    api.updateIndexConfig.mockResolvedValue({
      id: 'proj',
      config: configBody({ exclude_globs: ['**/.env'] }),
      reindex_required: true,
      catch_up_reindex: { started: false, reason: 'another reindex is already running' }
    });
    const chip = [...el.querySelectorAll('button')].find((b) =>
      (b.getAttribute('aria-label') ?? '').includes('**.min.js')
    );
    chip.click();
    flushSync();
    [...el.querySelectorAll('button')].find((b) => b.textContent.includes('Save changes')).click();
    await vi.waitFor(() => {
      flushSync();
      expect(el.textContent).toContain('did not start: another reindex is already running');
    });
  });
});
