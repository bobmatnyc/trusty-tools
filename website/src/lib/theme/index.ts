/**
 * Why: readers arrive with an OS preference and expect the site to honour it,
 * but also expect an explicit override to stick across navigations and
 * reloads. Keeping that decision in pure functions — rather than inside the
 * toggle component — is what makes it testable without mounting anything, and
 * lets the pre-paint snippet in `app.html` reuse the same key and the same
 * `.dark` class without importing the module graph.
 *
 * What: a three-mode preference ('light' | 'dark' | 'system') persisted to
 * localStorage under `STORAGE_KEY`. `system` is the default and resolves
 * through `prefers-color-scheme`. `applyTheme` is the single writer of the
 * `.dark` class on <html>, which is what `tailwind.config.js`'s
 * `darkMode: 'class'` and `app.css`'s `.dark` token block both key off — the
 * same mechanism as `crates/trusty-agents/ui/src/stores/theme.ts`.
 *
 * Every function is a no-op (or returns the default) when `window` is absent,
 * because every route here is prerendered and therefore runs this module in
 * Node at build time.
 *
 * Test: `theme.test.ts` — round-trips each mode, pins the storage key against
 * the copy inlined in `app.html`, and covers the rejected-storage path.
 */

export type ThemeMode = 'light' | 'dark' | 'system';

/** Shared with the pre-paint snippet inlined in `src/app.html`. */
export const STORAGE_KEY = 'foundry-theme';

export const THEME_MODES: readonly ThemeMode[] = ['light', 'dark', 'system'];

function isThemeMode(value: unknown): value is ThemeMode {
	return typeof value === 'string' && (THEME_MODES as readonly string[]).includes(value);
}

/** Reads the persisted preference, falling back to 'system'. */
export function readStoredTheme(): ThemeMode {
	if (typeof localStorage === 'undefined') return 'system';
	try {
		const stored = localStorage.getItem(STORAGE_KEY);
		return isThemeMode(stored) ? stored : 'system';
	} catch {
		// Private browsing and some embedded webviews throw on access.
		return 'system';
	}
}

/** True when the OS currently asks for a dark palette. */
export function prefersDark(): boolean {
	if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
	return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

/** Collapses the three-mode preference to the palette actually rendered. */
export function resolveTheme(mode: ThemeMode): 'light' | 'dark' {
	if (mode === 'system') return prefersDark() ? 'dark' : 'light';
	return mode;
}

/** Writes the `.dark` class on <html>. The only place that class is set. */
export function applyTheme(mode: ThemeMode): void {
	if (typeof document === 'undefined') return;
	document.documentElement.classList.toggle('dark', resolveTheme(mode) === 'dark');
}

/** Persists the preference and applies it. Storage failure must not break the toggle. */
export function persistTheme(mode: ThemeMode): void {
	try {
		localStorage?.setItem(STORAGE_KEY, mode);
	} catch {
		/* Preference simply will not survive the reload. */
	}
	applyTheme(mode);
}

/**
 * Re-applies the theme when the OS preference changes, but only while the
 * reader is on 'system'. Returns an unsubscribe function for `$effect`.
 */
export function watchSystemTheme(getMode: () => ThemeMode): () => void {
	if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return () => {};
	const query = window.matchMedia('(prefers-color-scheme: dark)');
	const onChange = () => {
		if (getMode() === 'system') applyTheme('system');
	};
	query.addEventListener('change', onChange);
	return () => query.removeEventListener('change', onChange);
}
