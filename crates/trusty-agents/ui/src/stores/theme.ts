import { writable, derived } from 'svelte/store';

/**
 * Why: Users expect modern web apps to respect their OS theme preference and
 * to offer an explicit override. Centralizing theme state in a single store
 * lets every component reactively switch styling without each one re-reading
 * localStorage or matchMedia, and the store survives Svelte's hot-reload
 * during dev because it's module-scoped.
 * What: Three-mode theme store ('light' | 'dark' | 'system') persisted to
 * localStorage. Toggles the `dark` class on <html> so Tailwind's
 * darkMode: 'class' utilities resolve correctly. Also tracks system
 * preference changes via matchMedia when in 'system' mode.
 * Test: Set theme to 'system', toggle OS dark mode → page flips palette.
 * Set theme to 'light', reload → page stays light. Set theme to 'dark',
 * reload → page stays dark.
 */
export type Theme = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'ompm-theme';

export function getInitialTheme(): Theme {
  if (typeof localStorage !== 'undefined') {
    const stored = localStorage.getItem(STORAGE_KEY) as Theme | null;
    if (stored === 'light' || stored === 'dark' || stored === 'system') return stored;
  }
  return 'system';
}

export const theme = writable<Theme>(getInitialTheme());

export const resolvedTheme = derived(theme, ($theme) => {
  if (typeof window === 'undefined') return 'dark';
  if ($theme === 'system') {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  return $theme;
});

export function applyTheme(t: Theme) {
  if (typeof window === 'undefined' || typeof document === 'undefined') return;
  const isDark =
    t === 'dark' ||
    (t === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.classList.toggle('dark', isDark);
}

export function setTheme(t: Theme) {
  theme.set(t);
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem(STORAGE_KEY, t);
  }
  applyTheme(t);
}

// Initialize on import (runs once per page load).
if (typeof window !== 'undefined') {
  applyTheme(getInitialTheme());
  // Track system preference changes when in 'system' mode so the page reacts
  // to OS-level light/dark toggles without requiring a reload.
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    theme.update((t) => {
      applyTheme(t);
      return t;
    });
  });
}
