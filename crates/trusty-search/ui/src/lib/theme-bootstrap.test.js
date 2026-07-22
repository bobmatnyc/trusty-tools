// Regression tests for the Foundry dark-theme activation bridge
// (issue #3487).
//
// Why: `theme-bootstrap.js` is the one place OS appearance turns into the
// `data-theme` attribute tokens.css's `[data-theme='dark']` block keys off.
// A silent regression here (attribute never set, or the change listener
// never wired) would leave the whole app pinned to light even when the OS
// is dark, with no visible error — worth pinning directly.
// What: Drives `applyTheme` against a real `document.documentElement`, and
// `initThemeBootstrap` against a mocked `matchMedia`.
// Test: this file — `pnpm test`.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { applyTheme, initThemeBootstrap } from './theme-bootstrap.js';

afterEach(() => {
  document.documentElement.removeAttribute('data-theme');
  vi.unstubAllGlobals();
});

describe('applyTheme', () => {
  it('sets data-theme="dark" when isDark is true', () => {
    applyTheme(true);
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('removes data-theme when isDark is false', () => {
    document.documentElement.setAttribute('data-theme', 'dark');
    applyTheme(false);
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});

describe('initThemeBootstrap', () => {
  it('applies the current match immediately and subscribes to changes', () => {
    let changeHandler;
    const query = {
      matches: true,
      addEventListener: vi.fn((event, handler) => {
        if (event === 'change') changeHandler = handler;
      }),
    };
    const matchMedia = vi.fn().mockReturnValue(query);
    vi.stubGlobal('matchMedia', matchMedia);

    initThemeBootstrap();

    expect(matchMedia).toHaveBeenCalledWith('(prefers-color-scheme: dark)');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(query.addEventListener).toHaveBeenCalledWith('change', expect.any(Function));

    changeHandler({ matches: false });
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });

  it('does nothing when matchMedia is unavailable', () => {
    vi.stubGlobal('matchMedia', undefined);
    expect(() => initThemeBootstrap()).not.toThrow();
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});
