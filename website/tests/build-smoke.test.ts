import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';

/**
 * Why: every other test here reads source files. None of them prove the site
 * actually BUILDS, and a Vercel deploy failure is the expensive way to find
 * that out. This runs the real production build once and asserts the artefacts
 * a Vercel deploy depends on — the Build Output API layout, the prerendered
 * landing page, and the self-hosted fonts.
 * What: shells out to `vite build`, then inspects `.vercel/output/`. Slow
 * (tens of seconds), which is why it lives in its own `smoke` Vitest project
 * with a long timeout rather than alongside the unit tests.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '..');
const OUTPUT = path.join(WEBSITE_ROOT, '.vercel/output');
const STATIC = path.join(OUTPUT, 'static');

let landingPage = '';

beforeAll(() => {
	rmSync(OUTPUT, { recursive: true, force: true });
	execFileSync('node', [path.join(WEBSITE_ROOT, 'node_modules/vite/bin/vite.js'), 'build'], {
		cwd: WEBSITE_ROOT,
		stdio: 'inherit'
	});
	landingPage = readFileSync(path.join(STATIC, 'index.html'), 'utf8');
});

describe('production build', () => {
	it('emits the Vercel Build Output API layout', () => {
		expect(existsSync(path.join(OUTPUT, 'config.json'))).toBe(true);
		expect(existsSync(STATIC)).toBe(true);
	});

	it('prerenders both routes to static HTML', () => {
		expect(existsSync(path.join(STATIC, 'index.html'))).toBe(true);
		expect(existsSync(path.join(STATIC, 'docs.html'))).toBe(true);
	});

	it('ships the self-hosted fonts and references no external font host', () => {
		expect(existsSync(path.join(STATIC, 'fonts/ibm-plex-sans-var.woff2'))).toBe(true);
		expect(existsSync(path.join(STATIC, 'fonts/OFL-IBMPlexSans.txt'))).toBe(true);
		expect(landingPage).not.toContain('fonts.googleapis.com');
		expect(landingPage).not.toContain('fonts.gstatic.com');
	});

	it('renders real landing-page content, not a shell', () => {
		expect(landingPage).toContain('trusty-search');
		expect(landingPage).toContain('Three flagship MCP servers');
		expect(landingPage).toContain('brew tap bobmatnyc/trusty');
	});

	it('sets the theme class before first paint', () => {
		// The anti-flash snippet must survive the build inlined in the HTML.
		expect(landingPage).toContain("classList.toggle('dark'");
	});
});
