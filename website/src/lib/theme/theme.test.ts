import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	STORAGE_KEY,
	applyTheme,
	persistTheme,
	readStoredTheme,
	resolveTheme,
	watchSystemTheme
} from './index';

/**
 * Why: the theme preference is written by two independent pieces of code — the
 * pre-paint snippet inlined in `app.html` and this module — and they agree
 * only by convention. A rename on one side produces a flash of the wrong
 * palette that no build gate would catch.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const APP_HTML = readFileSync(path.resolve(HERE, '../../app.html'), 'utf8');

function stubMatchMedia(matches: boolean) {
	const listeners = new Set<() => void>();
	vi.stubGlobal(
		'matchMedia',
		vi.fn(() => ({
			matches,
			addEventListener: (_: string, fn: () => void) => listeners.add(fn),
			removeEventListener: (_: string, fn: () => void) => listeners.delete(fn)
		}))
	);
	return listeners;
}

describe('theme preference', () => {
	beforeEach(() => {
		localStorage.clear();
		document.documentElement.classList.remove('dark');
		vi.unstubAllGlobals();
	});

	it('shares its storage key with the pre-paint snippet in app.html', () => {
		expect(APP_HTML).toContain(`localStorage.getItem('${STORAGE_KEY}')`);
	});

	it('defaults to system when nothing is stored', () => {
		expect(readStoredTheme()).toBe('system');
	});

	it('rejects a stored value that is not a mode', () => {
		localStorage.setItem(STORAGE_KEY, 'sepia');
		expect(readStoredTheme()).toBe('system');
	});

	it.each(['light', 'dark', 'system'] as const)('round-trips %s', (mode) => {
		persistTheme(mode);
		expect(localStorage.getItem(STORAGE_KEY)).toBe(mode);
		expect(readStoredTheme()).toBe(mode);
	});

	it('resolves system through prefers-color-scheme', () => {
		stubMatchMedia(true);
		expect(resolveTheme('system')).toBe('dark');
		stubMatchMedia(false);
		expect(resolveTheme('system')).toBe('light');
	});

	it('an explicit mode overrides the system preference', () => {
		stubMatchMedia(true);
		expect(resolveTheme('light')).toBe('light');
	});

	it('applyTheme is the only writer of the dark class', () => {
		stubMatchMedia(false);
		applyTheme('dark');
		expect(document.documentElement.classList.contains('dark')).toBe(true);
		applyTheme('light');
		expect(document.documentElement.classList.contains('dark')).toBe(false);
	});

	it('follows an OS change only while on system', () => {
		const listeners = stubMatchMedia(true);
		let mode: 'light' | 'dark' | 'system' = 'system';
		const stop = watchSystemTheme(() => mode);

		listeners.forEach((fn) => fn());
		expect(document.documentElement.classList.contains('dark')).toBe(true);

		mode = 'light';
		applyTheme('light');
		listeners.forEach((fn) => fn());
		expect(document.documentElement.classList.contains('dark')).toBe(false);

		stop();
		expect(listeners.size).toBe(0);
	});
});
