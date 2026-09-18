import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

import { RELEASED_FLAGSHIPS } from '../site';
import { findRepoRoot } from '../docs/repo';
import { ChangelogBuildError } from './errors';
import { buildChangelogSite, stripItems } from './site';

/**
 * Why: the four gates are the whole point of this module — a "What's New" that
 * renders empty is indistinguishable from one that says nothing shipped, so
 * each gate is provoked here rather than assumed.
 * What: a temp-repo fixture per gate. Each fixture starts from a WORKING repo
 * and breaks exactly one thing, so a green assertion means that one change
 * caused the failure. No case here reads the real corpus — those moved to
 * `site.corpus.test.ts`, which runs under its own vitest project and its own
 * CI check, so a changelog fragment no longer pays for this suite.
 * Test: this file; the real-corpus cases are `site.corpus.test.ts`.
 */

const REPO_ROOT = findRepoRoot();
const temps: string[] = [];

afterEach(() => {
	while (temps.length > 0) rmSync(temps.pop()!, { recursive: true, force: true });
});

/**
 * A minimal repository the builder accepts: the root marker `findRepoRoot`
 * looks for, plus one CHANGELOG.md per flagship. `overrides` replaces or (with
 * `null`) deletes one crate's file.
 */
function fixture(overrides: Record<string, string | null> = {}): string {
	const root = mkdtempSync(path.join(tmpdir(), 'trusty-changelog-'));
	temps.push(root);
	mkdirSync(path.join(root, 'docs'), { recursive: true });
	cpSync(
		path.join(REPO_ROOT, 'docs/public-manifest.tsv'),
		path.join(root, 'docs/public-manifest.tsv')
	);

	for (const flagship of RELEASED_FLAGSHIPS) {
		if (overrides[flagship.name] === null) continue;
		const file = path.join(root, 'crates', flagship.name, 'CHANGELOG.md');
		mkdirSync(path.dirname(file), { recursive: true });
		writeFileSync(
			file,
			overrides[flagship.name] ?? '## [1.0.0] — 2026-01-01\n\n### Added\n\n- a thing\n'
		);
	}
	return root;
}

const failuresOf = (run: () => unknown) => {
	try {
		run();
	} catch (error) {
		if (error instanceof ChangelogBuildError) return error.failures;
		throw error;
	}
	throw new Error('expected the build to fail, but it succeeded');
};

describe('the build gates', () => {
	it('passes on a repository where every flagship is populated', () => {
		expect(buildChangelogSite(fixture()).crates).toHaveLength(RELEASED_FLAGSHIPS.length);
	});

	it('fails when a flagship CHANGELOG.md is missing', () => {
		const failures = failuresOf(() => buildChangelogSite(fixture({ 'trusty-review': null })));
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-MISSING');
		expect(failures[0].file).toBe('crates/trusty-review/CHANGELOG.md');
	});

	it('fails when a flagship parses to zero releases', () => {
		const failures = failuresOf(() =>
			buildChangelogSite(fixture({ 'trusty-memory': '# Changelog\n\nNothing here yet.\n' }))
		);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-NO-RELEASES');
		expect(failures[0].file).toBe('crates/trusty-memory/CHANGELOG.md');
	});

	it('fails when a `## [` heading never closes its bracket', () => {
		const failures = failuresOf(() =>
			buildChangelogSite(fixture({ 'trusty-mpm': '## [1.0.0 — 2026-01-01\n\n### Added\n\n- a\n' }))
		);
		// The bad heading, then the crate it left with no releases at all.
		expect(failures.map((f) => f.code)).toEqual(['CHANGELOG-BAD-RELEASE', 'CHANGELOG-NO-RELEASES']);
		expect(failures[0].line).toBe(1);
	});

	it('fails when the newest release has no items', () => {
		const failures = failuresOf(() =>
			buildChangelogSite(
				fixture({
					'trusty-analyze':
						'## [2.0.0] — 2026-02-02\n\nVersion bump only.\n\n---\n\n## [1.0.0] — 2026-01-01\n\n### Added\n\n- a\n'
				})
			)
		);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-EMPTY-LATEST');
		expect(failures[0].problem).toContain('2.0.0');
	});

	it('reports every broken crate in one build rather than the first', () => {
		const failures = failuresOf(() =>
			buildChangelogSite(fixture({ 'trusty-review': null, 'trusty-memory': '# Changelog\n' }))
		);
		expect(failures.map((f) => f.code).sort()).toEqual([
			'CHANGELOG-MISSING',
			'CHANGELOG-NO-RELEASES'
		]);
	});

	/**
	 * The non-fatal half of the #5640 fix, end to end. A root-resolved link must
	 * keep the build green — refusing it is what took the public site down — and
	 * must reach the build log, because a resolution the build chose on the
	 * author's behalf that nobody can see is the finding this test exists for.
	 */
	it('logs a root-resolved link and still builds', () => {
		const root = fixture({
			'trusty-mpm':
				'## [1.0.0] — 2026-01-01\n\n### Added\n\n- see [ADR-0043](docs/adr/0043-cargo-bin-policy.md)\n'
		});
		const adr = path.join(root, 'docs/adr/0043-cargo-bin-policy.md');
		mkdirSync(path.dirname(adr), { recursive: true });
		writeFileSync(adr, '# ADR-0043\n');

		const logged: string[] = [];
		const original = console.warn;
		console.warn = (...args: unknown[]) => void logged.push(args.join(' '));
		try {
			const site = buildChangelogSite(root);
			expect(site.crates).toHaveLength(RELEASED_FLAGSHIPS.length);
		} finally {
			console.warn = original;
		}

		expect(logged).toHaveLength(1);
		expect(logged[0]).toContain('WARN CHANGELOG-ROOT-RELATIVE-LINK');
		expect(logged[0]).toContain('crates/trusty-mpm/CHANGELOG.md');
		expect(logged[0]).toContain('docs/adr/0043-cargo-bin-policy.md');
	});

	it('logs nothing when every link resolves beside its changelog', () => {
		const logged: string[] = [];
		const original = console.warn;
		console.warn = (...args: unknown[]) => void logged.push(args.join(' '));
		try {
			buildChangelogSite(fixture());
		} finally {
			console.warn = original;
		}
		expect(logged).toEqual([]);
	});

	it('does not fail on an unrecognised category, which is hand-written history', () => {
		const site = buildChangelogSite(
			fixture({ 'trusty-search': '## [1.0.0] — 2026-01-01\n\n### Highlights\n\n- a\n' })
		);
		expect(site.crates[0].latest.categories[0].label).toBe('Highlights');
	});
});

describe('the landing-page strip', () => {
	const release = (categories: [string, string[]][]) => ({
		version: '1.0.0',
		categories: categories.map(([label, texts]) => ({
			key: label.split(' ')[0],
			label,
			blocks: [],
			items: texts.map((text) => ({ html: text, text }))
		})),
		itemCount: categories.reduce((n, [, texts]) => n + texts.length, 0),
		line: 1
	});

	it('takes at most three items, tagged with the short bucket', () => {
		const lines = stripItems(
			release([
				['Fixed (closes #1373)', ['a', 'b']],
				['Added', ['c', 'd']]
			])
		);
		expect(lines).toEqual([
			{ category: 'Fixed', text: 'a' },
			{ category: 'Fixed', text: 'b' },
			{ category: 'Added', text: 'c' }
		]);
	});

	it('shows fewer when the release has fewer, and never pads', () => {
		expect(stripItems(release([['Added', ['only']]]))).toEqual([
			{ category: 'Added', text: 'only' }
		]);
	});

	// The same strip over the REAL corpus is `site.corpus.test.ts`'s
	// "produces a non-empty strip for every real flagship crate".
});
