import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import type { Element } from 'hast';

import { parsePage, stringifyHast } from '../docs/render';
import { findRepoRoot } from '../docs/repo';
import { parseChangelog, stripLinkDefinitions } from './parse';

/**
 * Why: the parser's two hardest claims can only be made against the REAL
 * corpus — that the link-reference trap is live in `trusty-search`'s 11k-line
 * file, and that no metavariable anywhere in the six-crate corpus is dropped.
 * Both walk `crates/*\/CHANGELOG.md`, so they are CORPUS cases: parsing the
 * corpus under the `unit` project timed out at 120_000 ms and took the whole
 * website suite red on a Rust-only PR (#8272).
 * What: the real trusty-search file for the trap, then a corpus-wide
 * metavariable sweep. Every fixture case stays in `parse.test.ts`.
 * Test: this file.
 */

const REPO_ROOT = findRepoRoot();
const readCrate = (name: string) =>
	readFileSync(path.join(REPO_ROOT, 'crates', name, 'CHANGELOG.md'), 'utf8');

/**
 * The probe `site.ts` builds for production: a real existence check against
 * this checkout. A case that parses a REAL changelog and asserts no failures
 * has to pass it — the default probe rejects everything, so the first live
 * relative link the corpus grows is reported as missing from the repository
 * (#6464: five `../../docs/adr/0032-…md` links in trusty-search 0.50.0).
 */
const repoProbe = (relative: string) => existsSync(path.join(REPO_ROOT, relative));

/** Every `## …` heading of `markdown`, serialised as HTML. */
function releaseHeadingHtml(markdown: string): string[] {
	return parsePage(markdown)
		.tree.children.filter(
			(node): node is Element => node.type === 'element' && node.tagName === 'h2'
		)
		.map((node) => stringifyHast(node.children));
}

describe('the link-reference trap in the real corpus', () => {
	// crates/trusty-search/CHANGELOG.md ends with 58 definitions pointing at
	// the PRE-MONOREPO repo. Their labels match the version headings exactly,
	// so remark silently turns every heading into a link to a dead repository.
	const source = readCrate('trusty-search');

	it('is real: the untouched file renders its version headings as anchors', () => {
		expect(source).toContain('[0.3.36]: https://github.com/bobmatnyc/trusty-search/compare');

		const trapped = releaseHeadingHtml(source);
		expect(trapped.some((html) => html.includes('<a href='))).toBe(true);
		expect(trapped.some((html) => html.includes('bobmatnyc/trusty-search/compare'))).toBe(true);
	});

	it('leaves every real trusty-search version heading as plain text, not an anchor', () => {
		// The newest heading, read straight from the untouched source — not a
		// pinned version string, which ages out on every trusty-search release
		// (#5417). Deriving the expectation from the same `source` that
		// `stripped` is computed from means the assertion tracks the corpus
		// instead of re-drifting from it on the next publish.
		const newestHeadingText = source.match(/^## (.+)$/m)?.[1];
		expect(newestHeadingText, 'no `## [version] — date` heading found at all').toBeDefined();

		const stripped = releaseHeadingHtml(stripLinkDefinitions(source));
		expect(stripped).not.toHaveLength(0);
		for (const html of stripped) {
			expect(html).not.toContain('<a ');
			expect(html).not.toContain('trusty-search/compare');
		}
		expect(stripped[0]).toBe(newestHeadingText);
	});

	/**
	 * The stripping assertion above proves `stripLinkDefinitions` works; this one
	 * proves `parseChangelog` actually USES it, which is the part that can
	 * regress. When a heading resolves into a link its brackets are consumed, so
	 * the version stops being separable from the date and reads
	 * `0.3.36 — 2026-05-14`.
	 */
	it('parses every real trusty-search version as a bare semver, brackets and all', () => {
		const { releases, failures } = parseChangelog(
			source,
			'crates/trusty-search/CHANGELOG.md',
			repoProbe
		);
		expect(failures).toEqual([]);
		expect(releases.length).toBeGreaterThan(100);
		for (const entry of releases) {
			expect(entry.version, `${entry.version} @ line ${entry.line}`).toMatch(/^\d+\.\d+\.\d+$/);
		}
	});
});

describe('metavariables in the real corpus', () => {
	it('keeps every metavariable in the real corpus, dropping none', () => {
		// The exact sites the gate found: each would otherwise publish a gap.
		const search = (crate: string, needle: string) => {
			const { releases } = parseChangelog(
				readCrate(crate),
				`crates/${crate}/CHANGELOG.md`,
				() => true
			);
			return releases.some((release) =>
				release.categories.some((category) =>
					category.items.some((entry) => entry.text.includes(needle))
				)
			);
		};
		expect(search('trusty-analyze', 'trusty-search index <path>')).toBe(true);
		expect(search('trusty-mpm', 'tm-<project>-<n>')).toBe(true);
		expect(search('trusty-mpm', 'tm session delete <id>')).toBe(true);
		expect(search('trusty-mpm', 'tm-<leaf>-NN')).toBe(true);
	});
});
