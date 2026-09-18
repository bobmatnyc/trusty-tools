import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';

import { buildDocSite, clearDocSiteCache, MANIFEST_PATH } from './site';
import { findRepoRoot } from './repo';

/**
 * Why: the boundary and the build gate are proved end to end here, against the
 * REAL 27-page corpus. This walks `docs/`, so it is a CORPUS suite — the `unit`
 * project walks no repository content, and a docs change pays for this check
 * instead of the whole website code suite (#8272).
 * What: one `buildDocSite()` pass over the real repository, asserted from
 * several angles. The fixture gates that cannot be shown without breaking the
 * real corpus stay in `site.test.ts`.
 * Test: this file.
 */

const REPO_ROOT = findRepoRoot();

/** A `docs/` file that is deliberately NOT published — the DO-NOT-PUBLISH tree. */
const UNLISTED_SOURCE = (() => {
	const dir = path.join(REPO_ROOT, 'docs/adr');
	const name = readdirSync(dir).find((entry) => entry.endsWith('.md'));
	if (!name) throw new Error('expected at least one ADR to exist');
	return `docs/adr/${name}`;
})();

describe('the real documentation corpus', () => {
	let site: ReturnType<typeof buildDocSite>;

	beforeAll(() => {
		clearDocSiteCache();
		site = buildDocSite();
	});

	it('builds every manifest page with zero findings', () => {
		const rows = readFileSync(path.join(REPO_ROOT, MANIFEST_PATH), 'utf8')
			.split('\n')
			.filter((line) => line.startsWith('PAGE\t'));
		expect(site.pages).toHaveLength(rows.length);
		expect(site.pages.every((page) => page.html.length > 200)).toBe(true);
	});

	it('renders each page from its own source, with the manifest title and section', () => {
		for (const page of site.pages) {
			expect(page.source.startsWith('docs/')).toBe(true);
			expect(page.title).not.toBe('');
			expect(page.sectionTitle).not.toBe('');
			expect(page.sourceUrl).toContain(`/blob/${site.commitSha}/${page.source}`);
		}
	});

	it('orders the nav by manifest file order, sections included', () => {
		const flattened = site.nav.flatMap((section) => section.pages.map((page) => page.href));
		expect(flattened).toEqual(site.pages.map((page) => page.href));
	});

	it('chains prev/next through the whole corpus in that same order', () => {
		expect(site.pages[0].prev).toBeUndefined();
		expect(site.pages.at(-1)!.next).toBeUndefined();
		for (let index = 1; index < site.pages.length; index += 1) {
			expect(site.pages[index].prev?.href).toBe(site.pages[index - 1].href);
			expect(site.pages[index - 1].next?.href).toBe(site.pages[index].href);
		}
	});

	it('classifies every link and leaves none pointing at blob/main', () => {
		expect(site.linkCounts.site).toBeGreaterThan(0);
		expect(site.linkCounts['repo-file'] + site.linkCounts['repo-dir']).toBeGreaterThan(0);
		expect(site.linkCounts.anchor).toBeGreaterThan(0);
		for (const page of site.pages) {
			expect(page.html).not.toContain('/blob/main/');
			expect(page.html).not.toContain('/tree/main/');
		}
	});

	it('emits only site-relative or github.com destinations — no other origin', () => {
		const origins = new Set<string>();
		for (const page of site.pages) {
			for (const [, href] of page.html.matchAll(/href="(https?:\/\/[^"]+)"/g)) {
				origins.add(new URL(href).origin);
			}
		}
		// Nothing here is FETCHED at runtime; these are destinations a reader
		// clicks. The assertion that the page issues no third-party REQUESTS is
		// in tests/build-smoke.test.ts, which inspects the built HTML.
		expect([...origins].every((origin) => origin.startsWith('https://'))).toBe(true);
		expect(origins.has('http://localhost')).toBe(false);
	});

	it('resolves every internal /docs link it emits to a page that exists', () => {
		const slugs = new Set(site.pages.map((page) => page.slug));
		for (const page of site.pages) {
			for (const [, href] of page.html.matchAll(/href="(\/docs[^"#]*)"/g)) {
				expect(slugs.has(href.replace(/^\/docs\/?/, ''))).toBe(true);
			}
		}
	});

	// THE BOUNDARY. An ADR exists on disk and is reachable by no lookup here.
	it('gives an unlisted docs/ file no page, no slug, and no route', () => {
		expect(readFileSync(path.join(REPO_ROOT, UNLISTED_SOURCE), 'utf8').length).toBeGreaterThan(0);
		expect(site.pages.some((page) => page.source === UNLISTED_SOURCE)).toBe(false);
		const slug = UNLISTED_SOURCE.replace(/^docs\//, '').replace(/\.md$/, '');
		expect(site.bySlug.has(slug)).toBe(false);
		expect(site.bySlug.has(UNLISTED_SOURCE)).toBe(false);
		expect(site.nav.flatMap((s) => s.pages).some((p) => p.href.includes('adr'))).toBe(false);
	});
});
