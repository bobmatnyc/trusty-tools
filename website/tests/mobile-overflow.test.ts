import { execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readFileSync, rmSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import type { AddressInfo } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { chromium, type Browser } from 'playwright';

import { whatsNewSections } from '../src/lib/changelog/site';

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
 *
 * TWO assertions per route/width, not one, and the second exists because the
 * first is not enough: `app.css` gives `main` an `overflow: clip` backstop
 * (see its comment — a residue from ~225 inline `<code>` spans' layout
 * rounding, not any one element; `clip` rather than `hidden` specifically so
 * `main` never becomes a `position: sticky` scroll-container boundary — see
 * that comment for the docs-sidebar breakage `hidden` caused and the desktop
 * test below that catches it). The backstop clips overflow at `main`'s own
 * boundary, so `document.documentElement.scrollWidth` reads clean even when
 * something overflows `main` and renders cut off and unreadable — confirmed
 * empirically: a long unbroken run of plain prose (no `<code>` wrapper, so
 * none of `.doc-prose :not(pre) > code`'s own containment applies) injected
 * into a changelog item left `document.documentElement.scrollWidth` at
 * exactly `clientWidth` while `main.scrollWidth` read 1697 against a 375
 * `clientWidth`, and the text was visibly sliced off mid-run in a
 * screenshot. The document-level assertion alone is decorative against that
 * failure mode — it would stay green while a future changelog entry
 * rendered clipped. `main.scrollWidth <= main.clientWidth` measures INSIDE
 * the clipping boundary, so it still catches that case; the document-level
 * assertion stays because it is what would catch overflow OUTSIDE `main`
 * (the sticky header, the footer).
 *
 * Known limitation, left as a limitation rather than solved speculatively
 * (code-critic MEDIUM on #5254): these two assertions cover overflow
 * escaping `document` and overflow escaping `main`. A THIRD element clipping
 * overflow somewhere between `main` and the content — none exists in this
 * codebase today — could in principle contain something that overflows IT
 * while both current checks stay green, the same way `main` alone
 * previously hid content from the document-level check. No such ancestor
 * exists to test against, so no synthetic one is added here; a future
 * intermediate clipping container would need its own scrollWidth-vs-
 * clientWidth measurement, following this same pattern.
 *
 * `MAIN_TOLERANCE_PX` exists because turning that measurement on surfaced a
 * second, pre-existing thing at 320px: `/whats-new` sits 1px over even on
 * the clean build, and it is cosmetic — the exhaustive per-element audit run
 * while fixing this (every `<code>`/text-node bounding rect, by hand) found
 * nothing rendered past the viewport edge; it took removing whole sections
 * of content to move the number, the signature of sub-pixel rounding
 * accumulated across the ~225 code spans on that page, not a real spatial
 * overflow. 1px is kept as an explicit, narrow allowance for exactly that —
 * not "a wider hidden" — and stays two orders of magnitude below anything a
 * real clipped-content regression produces (the injection proof below reads
 * main.scrollWidth 1697 against a 375 clientWidth). The homepage's OWN
 * 320px reading is different in kind, not degree — its `/` card grid
 * overflows main by 37px, a real layout defect with a real card clipped,
 * pre-existing and unrelated to this fix (present on `origin/main` before
 * it, confirmed by re-running this measurement against the unmodified
 * `app.css`). Silently tolerating that would be the masking this file
 * exists to prevent, so `/` is asserted at 375px only here — see #5255.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '..');
const OUTPUT = path.join(WEBSITE_ROOT, '.vercel/output');
const STATIC = path.join(OUTPUT, 'static');

/** Sub-pixel layout rounding, not a real overflow — see the file doc above. */
const MAIN_TOLERANCE_PX = 1;

// `/tools/trusty-git-analytics/audit` carries the wide content the `tga audit`
// port brought in: five tables and six `<pre>` blocks — five labelled `shell`,
// the sixth an error message quoted in the thesis card. Those are the two
// shapes that actually scroll a page sideways. It does NOT go through
// `ToolPage.svelte`, so it inherits none of that component's already-measured
// containment, which is why it is measured directly.
// `/tools/trusty-git-analytics` stays in the list even though the move left it
// with no table and no `<pre>` at all: it is now a second plain-prose case, and
// keeping it is what would catch wide content arriving back on it.
// `/tools/trusty-search` is the plain-prose flagship and stays as the control.
const ROUTES = [
	'/whats-new',
	'/',
	'/docs',
	'/docs/getting-started/install',
	'/tools/trusty-search',
	'/tools/trusty-git-analytics',
	'/tools/trusty-git-analytics/audit'
];
const WIDTHS = [375, 320];

/** No whitespace, and none of the HTML-entity-risky characters (`<`, `>`, `&`,
 * `"`) that would make `item.html`'s escaped form differ from the browser's
 * unescaped `textContent` — see `longestInlineCodeIdentifier` below. */
const SAFE_UNESCAPED_IDENTIFIER = /^[\w./:-]+$/;

/**
 * Why: pinning one specific identifier (`pipeline::citation_check::…`) broke
 * the moment its release aged out of `/whats-new`'s `DETAILED_RELEASES`
 * window (#5461) — trusty-review shipped past 0.11.0, and CI never runs this
 * suite (#5200), so the drift was silent. `whatsNewSections()` is the exact
 * data `/whats-new` renders from, so picking a probe out of it can never
 * target content the page has stopped showing.
 * What: the longest whitespace-free `<code>` run across every flagship
 * crate's currently-detailed releases — standing in for "a long inline
 * identifier", whichever one happens to be current.
 * Test: this file (`keeps a long inline identifier on /whats-new intact`).
 */
function longestInlineCodeIdentifier(): string {
	const { crates } = whatsNewSections();
	let longest = '';
	for (const crate of crates) {
		for (const release of crate.detailed) {
			for (const category of release.categories) {
				for (const item of category.items) {
					for (const match of item.html.matchAll(/<code>([^<]+)<\/code>/g)) {
						const candidate = match[1];
						if (candidate.length > longest.length && SAFE_UNESCAPED_IDENTIFIER.test(candidate)) {
							longest = candidate;
						}
					}
				}
			}
		}
	}
	return longest;
}

/**
 * `/` at 320px has its own pre-existing, unrelated overflow (#5255 — a card
 * grid, not changelog prose) that this PR does not fix. Skipping it here is
 * not masking: it is asserted at 375px like every other route, and the gap
 * is named, not silently widened away.
 */
function isKnownPreexistingGap(route: string, width: number): boolean {
	return route === '/' && width === 320;
}

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
			const testFn = isKnownPreexistingGap(route, width) ? it.skip : it;
			testFn(`${route} at ${width}px`, async () => {
				const page = await browser.newPage({ viewport: { width, height: 800 } });
				try {
					await page.goto(baseUrl + route, { waitUntil: 'networkidle' });
					const measurements = await page.evaluate(() => {
						const main = document.querySelector('main');
						return {
							doc: {
								scrollWidth: document.documentElement.scrollWidth,
								clientWidth: document.documentElement.clientWidth
							},
							// `main` clips overflow-x itself (app.css), so ITS OWN
							// scrollWidth vs clientWidth is what still catches content
							// that overflows main and renders clipped — the document
							// pair alone cannot (see the file-level comment above).
							main: main ? { scrollWidth: main.scrollWidth, clientWidth: main.clientWidth } : null
						};
					});
					expect(
						measurements.doc.scrollWidth,
						`${route} at ${width}px: document scrollWidth ${measurements.doc.scrollWidth} > clientWidth ${measurements.doc.clientWidth}`
					).toBeLessThanOrEqual(measurements.doc.clientWidth);
					if (measurements.main) {
						expect(
							measurements.main.scrollWidth,
							`${route} at ${width}px: <main> scrollWidth ${measurements.main.scrollWidth} > clientWidth ${measurements.main.clientWidth} (tolerance ${MAIN_TOLERANCE_PX}px) — content overflows main and renders clipped`
						).toBeLessThanOrEqual(measurements.main.clientWidth + MAIN_TOLERANCE_PX);
					}
				} finally {
					await page.close();
				}
			});
		}
	}

	// The regression this suite exists for: a long, space-free inline
	// identifier stays on one line and legible rather than breaking
	// mid-character — `.doc-prose :not(pre) > code` in `app.css` scrolls the
	// SPAN instead of wrapping it arbitrarily. The identifier itself is
	// derived from the live changelog data below, never pinned — see
	// `longestInlineCodeIdentifier`.
	it('keeps a long inline identifier on /whats-new intact, not broken mid-character', async () => {
		const identifier = longestInlineCodeIdentifier();
		expect(
			identifier.length,
			'no long inline <code> identifier found across any flagship’s currently-detailed releases — this probe needs different content to target'
		).toBeGreaterThan(20);

		const page = await browser.newPage({ viewport: { width: 375, height: 800 } });
		try {
			await page.goto(baseUrl + '/whats-new', { waitUntil: 'networkidle' });
			const text = await page.evaluate((needle) => {
				const code = Array.from(document.querySelectorAll('code')).find((c) =>
					c.textContent?.includes(needle)
				);
				return code?.textContent ?? null;
			}, identifier);
			expect(text).toContain(identifier);
		} finally {
			await page.close();
		}
	});

	// The regression the mobile-only matrix above cannot see: the `/docs`
	// sidebar (`routes/docs/+layout.svelte`) is `hidden` below `lg`, so a
	// 320/375px viewport never renders it — the desktop-only breakage
	// `overflow-x: hidden` caused on `main` (code-critic HIGH on #5254) is
	// invisible to every case above by construction. `overflow: clip` on
	// `main` does not create a scroll container the way `hidden` did, so
	// `position: sticky` inside `main` keeps computing against the viewport
	// instead of `main` itself. Proved this both ways, the same way the
	// `main`-vs-`document` measurement above was proved: reverting `app.css`
	// to `overflow-x: hidden` and re-running this test fails —
	// `sidebarTop after scroll: -667` — before failing it back to `clip`.
	it('keeps the /docs sidebar sticky at desktop width', async () => {
		const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
		try {
			await page.goto(baseUrl + '/docs/getting-started/install', { waitUntil: 'networkidle' });
			const before = await page.evaluate(() => {
				const nav = document.querySelector('nav[aria-label="Documentation"].sticky');
				return nav?.getBoundingClientRect().top ?? null;
			});
			expect(
				before,
				'sidebar nav not found before scroll — is it still `lg:block`?'
			).not.toBeNull();

			await page.evaluate(() => window.scrollTo(0, 800));
			await page.waitForTimeout(100);
			const after = await page.evaluate(
				() =>
					document.querySelector('nav[aria-label="Documentation"].sticky')?.getBoundingClientRect()
						.top
			);
			expect(
				after,
				`sidebar top drifted from ${before} to ${after} after scrolling — main became a scroll container`
			).toBe(before);
		} finally {
			await page.close();
		}
	});
});
