import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

import { DocBuildError } from './errors';
import { blobUrl, findRepoRoot, probeRepoEntry, resolveCommitSha, treeUrl } from './repo';

/**
 * Why: two things here can fail silently and produce a plausible-looking site
 * — a wrong repository root (reads the wrong tree) and a missing commit SHA
 * (tempts a `blob/main` fallback that silently retargets). Both must fail loud.
 * What: root discovery from a nested cwd, the SHA precedence chain, and the
 * refusal when no SHA can be established.
 * Test: this file.
 */

const SHA = 'c'.repeat(40);

function fixtureRepo(): string {
	const root = mkdtempSync(path.join(tmpdir(), 'trusty-docs-'));
	mkdirSync(path.join(root, 'docs/nested/deeper'), { recursive: true });
	writeFileSync(path.join(root, 'docs/public-manifest.tsv'), 'SECTION\ta\tA\n');
	writeFileSync(path.join(root, 'docs/nested/page.md'), '# Page\n');
	return root;
}

describe('findRepoRoot', () => {
	it('finds the repository root from a nested directory', () => {
		const root = fixtureRepo();
		expect(findRepoRoot(path.join(root, 'docs/nested/deeper'))).toBe(root);
	});

	it('fails with the Vercel remedy when no ancestor holds the manifest', () => {
		try {
			findRepoRoot(mkdtempSync(path.join(tmpdir(), 'not-a-repo-')));
			throw new Error('expected a failure');
		} catch (error) {
			expect(error).toBeInstanceOf(DocBuildError);
			const [failure] = (error as DocBuildError).failures;
			expect(failure.code).toBe('NO-REPO-ROOT');
			expect(failure.remedy).toContain('Include source files outside of the Root Directory');
		}
	});

	it('resolves the real repository root from this package', () => {
		expect(probeRepoEntry(findRepoRoot(), 'docs/public-manifest.tsv')).toBe('file');
	});
});

describe('probeRepoEntry', () => {
	it('distinguishes file, directory, and missing', () => {
		const root = fixtureRepo();
		expect(probeRepoEntry(root, 'docs/nested/page.md')).toBe('file');
		expect(probeRepoEntry(root, 'docs/nested')).toBe('dir');
		expect(probeRepoEntry(root, 'docs/nope.md')).toBe('missing');
	});
});

describe('resolveCommitSha', () => {
	it('prefers the platform environment over shelling out to git', () => {
		const root = fixtureRepo();
		expect(resolveCommitSha(root, { VERCEL_GIT_COMMIT_SHA: SHA })).toBe(SHA);
		expect(resolveCommitSha(root, { GITHUB_SHA: SHA })).toBe(SHA);
	});

	it('ignores a value that is not a 40-character SHA', () => {
		const root = fixtureRepo();
		expect(() => resolveCommitSha(root, { VERCEL_GIT_COMMIT_SHA: 'main' })).toThrow(DocBuildError);
	});

	it('reads HEAD from git when the environment carries nothing', () => {
		// This package IS in a checkout, so git answers here.
		expect(resolveCommitSha(findRepoRoot(), {})).toMatch(/^[0-9a-f]{40}$/);
	});

	it('refuses to guess when no SHA is available, rather than falling back to main', () => {
		const root = fixtureRepo();
		try {
			resolveCommitSha(root, {});
			throw new Error('expected a failure');
		} catch (error) {
			expect(error).toBeInstanceOf(DocBuildError);
			const [failure] = (error as DocBuildError).failures;
			expect(failure.code).toBe('NO-COMMIT-SHA');
			expect(failure.remedy).toContain('never fall back to `blob/main`');
		}
	});
});

describe('permalinks', () => {
	it('pins both file and directory links to the commit', () => {
		expect(blobUrl(SHA, 'docs/x.md', '#y')).toBe(
			`https://github.com/bobmatnyc/trusty-tools/blob/${SHA}/docs/x.md#y`
		);
		expect(treeUrl(SHA, 'docs/adr')).toBe(
			`https://github.com/bobmatnyc/trusty-tools/tree/${SHA}/docs/adr`
		);
	});
});
