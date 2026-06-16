// Regression tests for the API base-URL derivation (issue #1329).
//
// Why: The SPA is mounted at the daemon's `/ui/` route while the JSON API
// endpoints (/health, /indexes, …) are siblings at the daemon ROOT. A prior
// regression (PR #996) resolved API paths against document.baseURI verbatim,
// so at `…/ui/` a `/health` fetch hit `…/ui/health` (the SPA index.html,
// text/html) instead of `…/health` (JSON) — producing the offline badge and
// empty index list of #1329. These tests pin the corrected behaviour: the
// `ui/` mount segment is stripped so /health and /indexes resolve to the
// daemon origin, while the trusty-console reverse-proxy sub-path is preserved.
// What: Drives computeBase()/apiUrl() by stubbing document.baseURI to each of
// the served-at locations and asserting the resolved URLs.
// Test: this file — `pnpm test`.

import { afterEach, describe, expect, it, vi } from 'vitest';

// base.js snapshots the base at module-init time, so we must set
// document.baseURI BEFORE importing the module and re-import per scenario via
// vi.resetModules() + dynamic import.
async function loadWithBaseURI(baseURI) {
  vi.resetModules();
  vi.spyOn(document, 'baseURI', 'get').mockReturnValue(baseURI);
  delete window.__SEARCH_BASE__;
  return import('./base.js');
}

afterEach(() => {
  vi.restoreAllMocks();
  delete window.__SEARCH_BASE__;
});

describe('apiUrl — served directly by the daemon at /ui/ (issue #1329)', () => {
  it('resolves /health to the daemon origin, NOT under /ui/', async () => {
    const { apiUrl, apiBase } = await loadWithBaseURI('http://127.0.0.1:7878/ui/');
    expect(apiBase()).toBe('http://127.0.0.1:7878/');
    expect(apiUrl('/health')).toBe('http://127.0.0.1:7878/health');
  });

  it('resolves /indexes to the daemon origin, NOT under /ui/', async () => {
    const { apiUrl } = await loadWithBaseURI('http://127.0.0.1:7878/ui/');
    expect(apiUrl('/indexes')).toBe('http://127.0.0.1:7878/indexes');
  });

  it('resolves /ui/index.html the same way (index.html + ui/ both stripped)', async () => {
    const { apiUrl } = await loadWithBaseURI('http://127.0.0.1:7878/ui/index.html');
    expect(apiUrl('/health')).toBe('http://127.0.0.1:7878/health');
  });
});

describe('apiUrl — served at the bare root /', () => {
  it('resolves /health and /indexes to the origin', async () => {
    const { apiUrl, apiBase } = await loadWithBaseURI('http://127.0.0.1:7878/');
    expect(apiBase()).toBe('http://127.0.0.1:7878/');
    expect(apiUrl('/health')).toBe('http://127.0.0.1:7878/health');
    expect(apiUrl('/indexes')).toBe('http://127.0.0.1:7878/indexes');
  });
});

describe('apiUrl — served behind the trusty-console proxy at <prefix>/ui/', () => {
  it('preserves the proxy prefix and strips only the ui/ segment', async () => {
    const { apiUrl, apiBase } = await loadWithBaseURI(
      'https://console.local/proxy/search/ui/'
    );
    expect(apiBase()).toBe('https://console.local/proxy/search/');
    expect(apiUrl('/health')).toBe('https://console.local/proxy/search/health');
    expect(apiUrl('/indexes')).toBe('https://console.local/proxy/search/indexes');
  });
});

describe('apiUrl — window.__SEARCH_BASE__ override still wins', () => {
  it('honours an injected base global', async () => {
    vi.resetModules();
    window.__SEARCH_BASE__ = 'http://example.test:9000/custom';
    const { apiUrl } = await import('./base.js');
    expect(apiUrl('/health')).toBe('http://example.test:9000/custom/health');
  });
});
