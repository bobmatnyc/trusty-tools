import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';

import { RELEASED_FLAGSHIPS } from '../site';
import { findRepoRoot } from '../docs/repo';
import { ChangelogBuildError } from './errors';
import {
	buildChangelogSite,
	DETAILED_RELEASES,
	stripItems,
	whatsNewSections,
	type ChangelogSite
} from './site';

/**
 * Why: the four gates are the whole point of this module — a "What's New" that
 * renders empty is indistinguishable from one that says nothing shipped, so
 * each gate is provoked here rather than assumed. The real corpus is exercised
 * too, because the grammar deviations that would break it are in the files and
 * not in any fixture.
 * What: one pass over the released crates' real changelogs, then a temp-repo
 * fixture per gate. Each fixture starts from a WORKING repo and breaks exactly one thing,
 * so a green assertion means that one change caused the failure.
 * Test: this file.
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

describe('the real flagship-crate corpus', () => {
	let site: ChangelogSite;

	beforeAll(() => {
		site = buildChangelogSite(REPO_ROOT);
	});

	it('covers exactly the released flagships, in RELEASED_FLAGSHIPS order', () => {
		expect(site.crates.map((crate) => crate.name)).toEqual(RELEASED_FLAGSHIPS.map((f) => f.name));
	});

	it('gives every flagship at least one release with at least one item', () => {
		for (const crate of site.crates) {
			expect(crate.releases.length, crate.name).toBeGreaterThan(0);
			expect(crate.latest.itemCount, `${crate.name} ${crate.latest.version}`).toBeGreaterThan(0);
			expect(crate.latest).toBe(crate.releases[0]);
		}
	});

	it('parses the grammar deviations the corpus actually contains', () => {
		const byName = new Map(site.crates.map((crate) => [crate.name, crate]));
		const versions = (name: string) => byName.get(name)!.releases.map((r) => r.version);

		// A title where the date should be, and a heading with no separator.
		expect(versions('trusty-search')).toContain('0.1.46');
		expect(versions('trusty-mpm')).toContain('0.4.0');
		// A non-semver label, and a date sitting in the version slot.
		expect(versions('trusty-mpm')).toContain('consolidation');
		expect(versions('trusty-git-analytics')).toContain('2026-05-11');
	});

	it('links each crate at its LIVING changelog on main, not a pinned SHA', () => {
		for (const crate of site.crates) {
			expect(crate.sourceUrl).toBe(
				`https://github.com/bobmatnyc/trusty-tools/blob/main/crates/${crate.name}/CHANGELOG.md`
			);
		}
		expect(site.cratesDirUrl).toBe('https://github.com/bobmatnyc/trusty-tools/tree/main/crates');
	});

	/**
	 * `beforeAll` already throws if any link in the corpus fails to resolve, so
	 * this states what a green build means rather than adding new coverage: no
	 * relative link in the corpus escapes the repository or points at a
	 * missing path, and every one that survived is a `blob/main` link.
	 */
	it('resolves every relative link in the corpus, none escaping the repository', () => {
		const hrefs = site.crates.flatMap((crate) =>
			crate.releases.flatMap((release) =>
				[
					release.preambleHtml ?? '',
					...release.categories.flatMap((category) => [
						...category.items.map((entry) => entry.html),
						...category.blocks.map((block) => (block.kind === 'html' ? block.html : ''))
					])
				].flatMap((html) => [...html.matchAll(/href="([^"]*)"/g)].map((match) => match[1]))
			)
		);
		expect(hrefs.length).toBeGreaterThan(500);
		for (const href of hrefs) expect(href, href).toMatch(/^https?:\/\//);
		expect(hrefs.some((href) => href.includes('/blob/main/docs/specs/'))).toBe(true);
	});

	/**
	 * trusty-audit's CHANGELOG.md carries a real `## [0.6.0]` release, so it
	 * joined RELEASED_FLAGSHIPS (`Tool.released`) alongside the others. Only
	 * non-flagship crates such as `trusty-common` — never carded or paged —
	 * stay out of this surface.
	 */
	it('includes every released flagship, and only non-flagship crates stay out', () => {
		expect(site.crates.map((c) => c.name)).not.toContain('trusty-common');
		expect(site.crates.map((c) => c.name)).toContain('trusty-audit');
		expect(site.crates).toHaveLength(RELEASED_FLAGSHIPS.length);
	});
});

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

	it('does not fail on an unrecognised category, which is hand-written history', () => {
		const site = buildChangelogSite(
			fixture({ 'trusty-search': '## [1.0.0] — 2026-01-01\n\n### Highlights\n\n- a\n' })
		);
		expect(site.crates[0].latest.categories[0].label).toBe('Highlights');
	});
});

describe('the /whats-new projection', () => {
	it('splits every release into exactly one of detailed or earlier', () => {
		for (const crate of whatsNewSections(REPO_ROOT).crates) {
			expect(crate.detailed.length, crate.name).toBeGreaterThan(0);
			expect(crate.detailed.length).toBeLessThanOrEqual(DETAILED_RELEASES);
			expect(crate.detailed.length + crate.earlier.length).toBe(crate.releaseCount);

			const detailed = new Set(crate.detailed.map((release) => release.version));
			for (const summary of crate.earlier) expect(detailed.has(summary.version)).toBe(false);
		}
	});

	it('ships no item prose for a summarised release', () => {
		const crate = whatsNewSections(REPO_ROOT).crates.find((c) => c.name === 'trusty-search')!;
		expect(crate.earlier.length).toBeGreaterThan(100);
		for (const summary of crate.earlier) {
			expect(Object.keys(summary).sort()).toEqual(['date', 'title', 'version']);
		}
	});

	it('keeps the newest release detailed, so the page opens on what just shipped', () => {
		const projected = whatsNewSections(REPO_ROOT);
		const built = buildChangelogSite(REPO_ROOT);
		for (const [index, crate] of projected.crates.entries()) {
			expect(crate.detailed[0].version).toBe(built.crates[index].latest.version);
		}
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

	it('produces a non-empty strip for every real flagship crate', () => {
		for (const crate of buildChangelogSite(REPO_ROOT).crates) {
			const lines = stripItems(crate.latest);
			expect(lines.length, crate.name).toBeGreaterThan(0);
			expect(lines.length).toBeLessThanOrEqual(3);
			for (const line of lines) expect(line.text.trim(), crate.name).not.toBe('');
		}
	});
});
