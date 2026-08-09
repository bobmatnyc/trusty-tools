import { execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readFileSync, rmSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { chromium, type Browser } from 'playwright';

/**
 * Why: `tests/build-smoke.test.ts` proves the production build's HTML
 * contains the right text, but jsdom (the `unit` project's environment) has
 * no layout engine — it cannot compute `scrollWidth`, so it cannot see a page
 * scroll sideways. `/whats-new` did exactly that on the live site (471px of
 * `document.documentElement.scrollWidth` against a 375px viewport, from
 * inline `<code>` identifiers with no space to wrap at), and nothing in this
 * suite would have caught it before a real browser measured it. This file is
 * that measurement: a real Chromium (already cached locally by the
 * `@playwright/test` install under `crates/trusty-agents/ui`, so this adds no
 * new browser download) loads the PRODUCTION build's static output and reads
 * `scrollWidth`/`clientWidth` the same way a phone would.
 * What: serves `.vercel/output/static` from a plain Node HTTP server (the
 * adapter's clean-URL convention — `/whats-new` -> `whats-new.html`,
 * `/docs/x` -> `docs/x.html` — mirrors `routeToArtifact` in
 * `build-smoke.test.ts`) and asserts, at 320px and 375px, that no page's
 * `<html>` scrolls horizontally. 320px is deliberately narrower than the
 * 375px the defect was measured at, so a fix tuned to exactly one width
 * cannot pass by luck.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '..');
const OUTPUT = path.join(WEBSITE_ROOT, '.vercel/output');
const STATIC = path.join(OUTPUT, 'static');

const ROUTES = [
	'/whats-new',
	'/',
	'/docs',
	'/docs/getting-started/install',
	'/tools/trusty-search'
];
const WIDTHS = [375, 320];

/** `/whats-new` -> `whats-new.html`; `/` -> `index.html`; `/a/b` -> `a/b.html`. */
function routeToFile(route: string): string {
	if (route === '/') return 'index.html';
	return `${route.slice(1)}.html`;
}

const CONTENT_TYPES: Record<string, string> = {
	'.html': 'text/html',
	'.css': 'text/css',
	'.js': 'text/javascript',
	'.woff2': 'font/woff2',
	'.svg': 'image/svg+xml',
	'.json': 'application/json'
};

/**
 * Why: a route that only maps clean URLs to their HTML file (the adapter's
 * own convention) serves a page with every CSS/JS subresource 404ing — the
 * page then renders in the browser's UA stylesheet, not Foundry's, which
 * changes `scrollWidth` by hundreds of pixels and has nothing to do with the
 * defect this file measures. Static assets under `_app/`, `fonts/`, etc. are
 * served from their literal path FIRST; only an extension-less request falls
 * back to the clean-URL HTML mapping.
 */
function resolveStaticFile(url: string): string | null {
	const clean = url.split('?')[0];
	const literal = path.join(STATIC, clean);
	if (existsSync(literal) && statSync(literal).isFile()) return literal;
	const mapped = path.join(STATIC, routeToFile(clean));
	if (existsSync(mapped)) return mapped;
	return null;
}

let server: ReturnType<typeof createServer>;
let baseUrl: string;
let browser: Browser;

beforeAll(async () => {
	// Independent of `build-smoke.test.ts`'s own build — Vitest gives each
	// test file no ordering guarantee, so this can't assume that file's
	// `beforeAll` already ran, or that `.vercel/output` is still the build it
	// left behind. Cleared for the same reason build-smoke.test.ts clears it:
	// `adapter-vercel` symlinks a function's `node_modules` on generation and
	// errors EEXIST re-running into a directory that already has one. Both
	// smoke files building the SAME fixed `.vercel/output` path is also why
	// `vite.config.ts` sets `fileParallelism: false` on this project — two
	// concurrent builds would race on this same rmSync/build sequence instead
	// of just re-running it safely in turn.
	rmSync(OUTPUT, { recursive: true, force: true });

	// Same production-mode build build-smoke.test.ts uses, for the same
	// reason: `vite build` alone reads `NODE_ENV` from the Vitest process,
	// which is `test`, not `production`.
	execFileSync('node', [path.join(WEBSITE_ROOT, 'node_modules/vite/bin/vite.js'), 'build'], {
		cwd: WEBSITE_ROOT,
		stdio: 'inherit',
		env: { ...process.env, NODE_ENV: 'production' }
	});

	server = createServer((req, res) => {
		const file = resolveStaticFile(req.url ?? '/');
		if (!file) {
			res.writeHead(404);
			res.end('not found');
			return;
		}
		const contentType = CONTENT_TYPES[path.extname(file)] ?? 'application/octet-stream';
		res.writeHead(200, { 'content-type': contentType });
		res.end(readFileSync(file));
	});
	await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
	const { port } = server.address() as AddressInfo;
	baseUrl = `http://127.0.0.1:${port}`;
	browser = await chromium.launch();
}, 60_000);

afterAll(async () => {
	await browser?.close();
	await new Promise<void>((resolve) => server.close(() => resolve()));
});

describe('no page scrolls horizontally on mobile', () => {
	for (const width of WIDTHS) {
		for (const route of ROUTES) {
			it(`${route} at ${width}px`, async () => {
				const page = await browser.newPage({ viewport: { width, height: 800 } });
				try {
					await page.goto(baseUrl + route, { waitUntil: 'networkidle' });
					const { scrollWidth, clientWidth } = await page.evaluate(() => ({
						scrollWidth: document.documentElement.scrollWidth,
						clientWidth: document.documentElement.clientWidth
					}));
					expect(
						scrollWidth,
						`${route} at ${width}px: scrollWidth ${scrollWidth} > clientWidth ${clientWidth}`
					).toBeLessThanOrEqual(clientWidth);
				} finally {
					await page.close();
				}
			});
		}
	}

	// The regression this suite exists for: a long, space-free inline
	// identifier stays on one line and legible rather than breaking
	// mid-character — `.doc-prose :not(pre) > code` in `app.css` scrolls the
	// SPAN instead of wrapping it arbitrarily.
	it('keeps a long inline identifier on /whats-new intact, not broken mid-character', async () => {
		const page = await browser.newPage({ viewport: { width: 375, height: 800 } });
		try {
			await page.goto(baseUrl + '/whats-new', { waitUntil: 'networkidle' });
			const text = await page.evaluate(() => {
				const code = Array.from(document.querySelectorAll('code')).find((c) =>
					c.textContent?.includes('downgrade_uncitable_findings')
				);
				return code?.textContent ?? null;
			});
			expect(text).toContain('pipeline::citation_check::downgrade_uncitable_findings');
		} finally {
			await page.close();
		}
	});
});
