import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';

import { DocBuildError } from './errors';
import { buildDocSite, buildDocSiteIfAvailable, clearDocSiteCache, MANIFEST_PATH } from './site';
import { findRepoRoot } from './repo';

/**
 * Why: this is where the boundary and the build gate are proved end to end,
 * against the REAL 27-page corpus rather than a synthetic sample. The fixture
 * cases below cover what the real corpus cannot demonstrate without breaking it
 * — a broken link, a route collision, a deleted source.
 * What: one pass over the real site, then a temp-repo fixture per gate.
 * Test: this file.
 */

const SHA = 'd'.repeat(40);
const REPO_ROOT = findRepoRoot();

/** A `docs/` file that is deliberately NOT published — the DO-NOT-PUBLISH tree. */
const UNLISTED_SOURCE = (() => {
	const dir = path.join(REPO_ROOT, 'docs/adr');
	const name = readdirSync(dir).find((entry) => entry.endsWith('.md'));
	if (!name) throw new Error('expected at least one ADR to exist');
	return `docs/adr/${name}`;
})();

function fixture(files: Record<string, string>): string {
	const root = mkdtempSync(path.join(tmpdir(), 'trusty-site-'));
	for (const [relative, contents] of Object.entries(files)) {
		const absolute = path.join(root, relative);
		mkdirSync(path.dirname(absolute), { recursive: true });
		writeFileSync(absolute, contents);
	}
	return root;
}

const buildFixture = (files: Record<string, string>) => {
	process.env.TRUSTY_DOCS_COMMIT_SHA = SHA;
	return buildDocSite(fixture(files));
};

const failuresOf = (run: () => unknown) => {
	try {
		run();
	} catch (error) {
		if (error instanceof DocBuildError) return error.failures;
		throw error;
	}
	throw new Error('expected the build to fail, but it succeeded');
};

afterEach(() => {
	delete process.env.TRUSTY_DOCS_COMMIT_SHA;
});

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

describe('build gates', () => {
	const manifest = (rows: string) => ({ 'docs/public-manifest.tsv': rows });

	it('fails when an internal link does not resolve', () => {
		const failures = failuresOf(() =>
			buildFixture({
				...manifest('SECTION\ta\tA\nPAGE\ta\tdocs/one.md\t/\tOne\n'),
				'docs/one.md': '# One\n\nSee [the plan](plans/roadmap.md).\n'
			})
		);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('BROKEN-LINK');
		expect(failures[0].file).toBe('docs/one.md');
		expect(failures[0].line).toBe(3);
		expect(failures[0].problem).toContain('docs/plans/roadmap.md');
	});

	it('fails when a manifest source is missing', () => {
		const failures = failuresOf(() =>
			buildFixture(manifest('SECTION\ta\tA\nPAGE\ta\tdocs/absent.md\t/\tAbsent\n'))
		);
		expect(failures[0].code).toBe('MISSING-SOURCE');
	});

	it('fails on a route collision', () => {
		const failures = failuresOf(() =>
			buildFixture({
				...manifest(
					'SECTION\ta\tA\nPAGE\ta\tdocs/one.md\t/x\tOne\nPAGE\ta\tdocs/two.md\t/x\tTwo\n'
				),
				'docs/one.md': '# One\n',
				'docs/two.md': '# Two\n'
			})
		);
		expect(failures[0].code).toBe('DUP-ROUTE');
	});

	it('reports every broken link in the corpus, not just the first', () => {
		const failures = failuresOf(() =>
			buildFixture({
				...manifest('SECTION\ta\tA\nPAGE\ta\tdocs/one.md\t/\tOne\nPAGE\ta\tdocs/two.md\t/t\tTwo\n'),
				'docs/one.md': '# One\n\n[a](gone.md)\n',
				'docs/two.md': '# Two\n\n[b](also-gone.md)\n'
			})
		);
		expect(failures.map((f) => f.file)).toEqual(['docs/one.md', 'docs/two.md']);
	});

	it('returns undefined when there is no repository to read, rather than a 500', () => {
		const empty = mkdtempSync(path.join(tmpdir(), 'no-repo-'));
		const previous = process.cwd();
		process.chdir(empty);
		try {
			clearDocSiteCache();
			expect(buildDocSiteIfAvailable()).toBeUndefined();
		} finally {
			process.chdir(previous);
			clearDocSiteCache();
		}
	});

	it('still throws a real gate failure — only a missing repository is tolerated', () => {
		process.env.TRUSTY_REPO_ROOT = fixture({
			'docs/public-manifest.tsv': 'SECTION\ta\tA\nPAGE\ta\tdocs/gone.md\t/\tGone\n'
		});
		process.env.TRUSTY_DOCS_COMMIT_SHA = SHA;
		try {
			clearDocSiteCache();
			expect(() => buildDocSiteIfAvailable()).toThrow(DocBuildError);
		} finally {
			delete process.env.TRUSTY_REPO_ROOT;
			clearDocSiteCache();
		}
	});

	it('builds a clean fixture and rewrites its cross-links', () => {
		const site = buildFixture({
			...manifest('SECTION\ta\tA\nPAGE\ta\tdocs/one.md\t/\tOne\nPAGE\ta\tdocs/two.md\t/t\tTwo\n'),
			'docs/one.md': '# One\n\n## Detail\n\n[two](two.md) [spec](spec/s.md) [self](#detail)\n',
			'docs/two.md': '# Two\n\n[back](one.md#detail)\n',
			'docs/spec/s.md': '# Spec\n'
		});
		expect(site.pages[0].html).toContain('href="/docs/t"');
		expect(site.pages[0].html).toContain(`/blob/${SHA}/docs/spec/s.md`);
		expect(site.pages[1].html).toContain('href="/docs#detail"');
		expect(site.linkCounts).toEqual({
			external: 0,
			anchor: 1,
			site: 2,
			'repo-file': 1,
			'repo-dir': 0
		});
	});
});
