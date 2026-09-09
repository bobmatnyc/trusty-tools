// Why: `apiBase()` decides where every fetch() and every EventSource in this app
// goes. Before #6637 this file's job was to pin a hardcoded port literal against
// the Rust constant it copied — the bridge binds an ephemeral port instead, so
// there is no literal left to pin and the risk moved: a web-mode fallback that
// invented a URL would send this app's requests, credential attached, to
// whatever answers at that address.
// What: asserts Tauri mode asks the Rust side and web mode invents nothing.
// Test: this file.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { BASE_URL_STORAGE_KEY, apiBase, isTauri } from './api-config';

describe('apiBase in a plain browser tab', () => {
  beforeEach(() => {
    localStorage.clear();
    // The Tauri probe reads `window.__TAURI_INTERNALS__`; jsdom has none, so
    // these cases are web mode by construction.
    expect(isTauri()).toBe(false);
  });

  it('invents no URL when nothing is configured', async () => {
    await expect(apiBase()).resolves.toBe('');
  });

  it('reads the operator-set override', async () => {
    localStorage.setItem(BASE_URL_STORAGE_KEY, 'http://127.0.0.1:54321');
    await expect(apiBase()).resolves.toBe('http://127.0.0.1:54321');
  });

  it('treats blocked site data as unconfigured rather than throwing', async () => {
    const getItem = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('site data blocked');
    });
    await expect(apiBase()).resolves.toBe('');
    getItem.mockRestore();
  });
});
