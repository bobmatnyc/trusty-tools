import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

import { DocBuildError } from './errors';
import { buildDocSite, buildDocSiteIfAvailable, clearDocSiteCache } from './site';

/**
 * Why: the build gate is proved here on temp-repo fixtures — a broken link, a
 * route collision, a deleted source — which the real corpus cannot demonstrate
 * without breaking it.
 * What: a temp-repo fixture per gate. Nothing here reads the repository: the
 * real-corpus pass lives in `site.corpus.test.ts` and runs as its own CI check.
 * Test: this file.
 */

const SHA = 'd'.repeat(40);

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
