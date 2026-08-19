import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import type { Element } from 'hast';

import { parsePage, stringifyHast } from '../docs/render';
import { findRepoRoot } from '../docs/repo';
import { categoryKey, parseChangelog, parseReleaseHeading, stripLinkDefinitions } from './parse';

/**
 * Why: every case below is a shape the REAL corpus contains, or a malformed
 * input the gate must reject. Cases that only prove the happy path on
 * synthetic input would not have caught the link-reference trap, which is the
 * defect this parser exists to avoid.
 * What: fixture cases for each grammar deviation, then the real trusty-search
 * file for the trap itself.
 * Test: this file.
 */

const REPO_ROOT = findRepoRoot();
const readCrate = (name: string) =>
	readFileSync(path.join(REPO_ROOT, 'crates', name, 'CHANGELOG.md'), 'utf8');

const parse = (markdown: string) => parseChangelog(markdown, 'crates/x/CHANGELOG.md');

/** Every `## …` heading of `markdown`, serialised as HTML. */
function releaseHeadingHtml(markdown: string): string[] {
	return parsePage(markdown)
		.tree.children.filter(
			(node): node is Element => node.type === 'element' && node.tagName === 'h2'
		)
		.map((node) => stringifyHast(node.children));
}

describe('the link-reference trap', () => {
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
	 * The assertion above proves `stripLinkDefinitions` works; this one proves
	 * `parseChangelog` actually USES it, which is the part that can regress.
	 * When a heading resolves into a link its brackets are consumed, so the
	 * version stops being separable from the date and reads `0.3.36 — 2026-05-14`.
	 */
	it('parses every real trusty-search version as a bare semver, brackets and all', () => {
		const { releases, failures } = parseChangelog(source, 'crates/trusty-search/CHANGELOG.md');
		expect(failures).toEqual([]);
		expect(releases.length).toBeGreaterThan(100);
		for (const entry of releases) {
			expect(entry.version, `${entry.version} @ line ${entry.line}`).toMatch(/^\d+\.\d+\.\d+$/);
		}
	});

	it('recovers the version the trap would otherwise corrupt', () => {
		const heading = '## [0.3.36] — 2026-05-14\n\n### Fixed\n\n- a\n';
		const definition = '\n[0.3.36]: https://example.invalid/x\n';

		// Without stripping, the brackets are eaten by the link and the version
		// is no longer separable from the date.
		expect(parseReleaseHeading('0.3.36 — 2026-05-14')?.version).toBe('0.3.36 — 2026-05-14');
		// With it — which is what `parseChangelog` always does — it is exact.
		expect(parseChangelog(heading + definition, 'f.md').releases[0].version).toBe('0.3.36');
	});

	it('leaves a definition-shaped line inside a fenced block alone', () => {
		const fenced = '```md\n[0.1.0]: https://example.invalid/x\n```\n';
		expect(stripLinkDefinitions(fenced)).toBe(fenced);
	});

	it('blanks the definition rather than deleting it, so line numbers hold', () => {
		const { releases } = parse('[a]: https://example.invalid/a\n\n## [1.0.0] — 2026-01-01\n');
		expect(releases[0].line).toBe(3);
	});
});

describe('release headings', () => {
	it('parses the em-dash form the assembler emits', () => {
		expect(parseReleaseHeading('[0.42.3] — 2026-08-05')).toEqual({
			version: '0.42.3',
			date: '2026-08-05',
			title: undefined
		});
	});

	it('accepts an en dash and a hyphen as the separator', () => {
		expect(parseReleaseHeading('[1.0.0] – 2026-08-05')?.date).toBe('2026-08-05');
		expect(parseReleaseHeading('[1.0.0] - 2026-08-05')?.date).toBe('2026-08-05');
	});

	it('keeps a title where the date should be, rather than dropping the release', () => {
		// 49 real headings look like this, 48 of them in trusty-search.
		expect(parseReleaseHeading('[0.1.46] — 4 indexing speed optimizations')).toEqual({
			version: '0.1.46',
			date: undefined,
			title: '4 indexing speed optimizations'
		});
	});

	it('keeps a trailing marker alongside the date', () => {
		expect(parseReleaseHeading('[0.11.0] — 2026-05-26  BREAKING')).toEqual({
			version: '0.11.0',
			date: '2026-05-26',
			title: 'BREAKING'
		});
	});

	it('accepts a non-semver label and a missing separator', () => {
		expect(parseReleaseHeading('[consolidation] — 2026-05-26')?.version).toBe('consolidation');
		expect(parseReleaseHeading('[0.4.0] and prior')).toEqual({
			version: '0.4.0',
			date: undefined,
			title: 'and prior'
		});
	});

	it('reads a date sitting in the version slot', () => {
		expect(parseReleaseHeading('[2026-05-11]')).toEqual({
			version: '2026-05-11',
			date: '2026-05-11',
			title: undefined
		});
	});

	it('fails the build on a `## [` line that never closes its bracket', () => {
		expect(parseReleaseHeading('[0.1.0 — 2026-01-01')).toBeUndefined();
		expect(parseReleaseHeading('[]')).toBeUndefined();

		const { releases, failures } = parse('## [0.1.0 — 2026-01-01\n\n### Added\n\n- thing\n');
		expect(releases).toEqual([]);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-BAD-RELEASE');
		expect(failures[0].line).toBe(1);
		expect(failures[0].remedy).toContain('[<version>]');
	});

	it('accepts the same heading once the bracket is closed', () => {
		const { releases, failures } = parse('## [0.1.0] — 2026-01-01\n\n### Added\n\n- thing\n');
		expect(failures).toEqual([]);
		expect(releases).toHaveLength(1);
		expect(releases[0].itemCount).toBe(1);
	});
});

describe('category headings', () => {
	it('buckets a qualified heading by its leading word and keeps the full label', () => {
		const { releases } = parse(
			'## [1.0.0] — 2026-01-01\n\n### Fixed (closes #1373)\n\n- a thing\n'
		);
		const [category] = releases[0].categories;
		expect(category.key).toBe('Fixed');
		expect(category.label).toBe('Fixed (closes #1373)');
		expect(category.items).toHaveLength(1);
	});

	it('handles the other qualifier shapes the corpus actually writes', () => {
		expect(categoryKey('Added — Phase 2 (language-specific static enrichment)')).toBe('Added');
		expect(categoryKey('Deprecated: verbose managed session-lifecycle verbs (issue #1205)')).toBe(
			'Deprecated'
		);
		expect(categoryKey('BREAKING')).toBe('Breaking');
		expect(categoryKey('Changed:')).toBe('Changed');
	});

	it('renders an unrecognised category under its literal heading, without failing', () => {
		const { releases, failures } = parse('## [1.0.0] — 2026-01-01\n\n### Highlights\n\n- a\n');
		expect(failures).toEqual([]);
		expect(releases[0].categories[0]).toMatchObject({ key: 'Highlights', label: 'Highlights' });
	});
});

describe('items', () => {
	it('preserves inline markdown as HTML and offers the same content as text', () => {
		const { releases } = parse(
			'## [1.0.0] — 2026-01-01\n\n### Fixed\n\n- **Bold** `code` and a [link](https://example.com/a)\n'
		);
		const [item] = releases[0].categories[0].items;
		expect(item.html).toContain('<strong>Bold</strong>');
		expect(item.html).toContain('<code>code</code>');
		expect(item.html).toContain('href="https://example.com/a"');
		expect(item.text).toBe('Bold code and a link');
	});

	it('keeps a category’s prose and tables in source order around its list', () => {
		const { releases } = parse(
			'## [1.0.0] — 2026-01-01\n\n### Changed\n\nLead-in.\n\n- a\n\n| H |\n| --- |\n| c |\n'
		);
		const kinds = releases[0].categories[0].blocks.map((block) => block.kind);
		expect(kinds).toEqual(['html', 'items', 'html']);
	});

	it('keeps a release preamble written before the first category', () => {
		const { releases } = parse(
			'## [0.22.0] — 2026-07-27\n\nMINOR, not the patch this was staged as.\n\n### Changed\n\n- a\n'
		);
		expect(releases[0].preambleHtml).toContain('MINOR, not the patch');
		expect(releases[0].itemCount).toBe(1);
	});

	it('keeps a version-bump-only release rather than rendering a bare heading', () => {
		// crates/trusty-search/CHANGELOG.md `## [0.3.34]` is exactly this shape.
		const { releases, failures } = parse('## [0.3.34] — 2026-05-13\n\n_(version bump only)_\n');
		expect(failures).toEqual([]);
		expect(releases[0].itemCount).toBe(0);
		expect(releases[0].preambleHtml).toContain('version bump only');
	});
});

describe('links and metavariables inside an item', () => {
	const withProbe = (markdown: string, exists: string[]) =>
		parseChangelog(markdown, 'crates/trusty-mpm/CHANGELOG.md', (relative) =>
			exists.includes(relative)
		);

	const item = (body: string) => `## [1.0.0] — 2026-01-01\n\n### Added\n\n- ${body}\n`;

	it('rewrites a relative link into a blob/main link on the crate’s own path', () => {
		const { releases, failures } = withProbe(item('see [spec](../../docs/specs/foo.md)'), [
			'docs/specs/foo.md'
		]);
		expect(failures).toEqual([]);
		expect(releases[0].categories[0].items[0].html).toContain(
			'href="https://github.com/bobmatnyc/trusty-tools/blob/main/docs/specs/foo.md"'
		);
	});

	/**
	 * The regression this pins: an earlier revision stripped the leading `../`
	 * and accepted the result whenever that path existed, so a link escaping
	 * the repository became a confident link to whatever it happened to land
	 * on. The probe says the target EXISTS at the repo root, which is exactly
	 * the condition that made the old code fail open — the build must still
	 * reject it, because the link as written does not resolve there.
	 */
	it('fails the build on a link that escapes the repository, even when the stripped path exists', () => {
		const { releases, failures } = withProbe(item('[the README](../../../README.md)'), [
			'README.md',
			'crates/README.md'
		]);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-BAD-LINK');
		expect(failures[0].problem).toContain('outside the repository');
		// And nothing was emitted: no guessed href reached the rendered item.
		expect(releases[0].categories[0].items[0].html).not.toContain('blob/main');
	});

	it('fails on any depth of escape, not just one level too many', () => {
		for (const href of ['../../../x.md', '../../../../../../x.md', '../../../a/../x.md']) {
			const { failures } = withProbe(item(`[x](${href})`), ['x.md', 'a/x.md']);
			expect(
				failures.map((f) => f.code),
				href
			).toEqual(['CHANGELOG-BAD-LINK']);
		}
	});

	/**
	 * The exact entry `d1774c81` added, replayed verbatim. Every Vercel deploy
	 * from that commit on failed on it: `docs/adr/0043-cargo-bin-policy.md` is
	 * the path from the repository root, and the checker only ever tried
	 * `crates/trusty-mpm/docs/adr/…` (#5640).
	 */
	it('resolves a repo-root-relative link that does not exist beside the changelog', () => {
		const { releases, failures } = withProbe(
			item('see [ADR-0043](docs/adr/0043-cargo-bin-policy.md)'),
			['docs/adr/0043-cargo-bin-policy.md']
		);
		expect(failures).toEqual([]);
		expect(releases[0].categories[0].items[0].html).toContain(
			'href="https://github.com/bobmatnyc/trusty-tools/blob/main/docs/adr/0043-cargo-bin-policy.md"'
		);
	});

	// Precedence, not preference: the fallback fires only where the first
	// reading found nothing, so a link that already resolved beside the
	// changelog keeps resolving there even when the same path exists at the
	// root. Without this the #5640 widening would silently re-point links.
	it('prefers the changelog-relative reading when both readings exist', () => {
		const { releases, failures } = withProbe(item('see [readme](README.md)'), [
			'crates/trusty-mpm/README.md',
			'README.md'
		]);
		expect(failures).toEqual([]);
		expect(releases[0].categories[0].items[0].html).toContain(
			'blob/main/crates/trusty-mpm/README.md'
		);
	});

	// An href that states its base explicitly gets one reading. `./` and `../`
	// mean "from here", so a root file with the same name must not rescue them.
	it('does not fall back to the repository root for an explicitly relative link', () => {
		for (const href of ['./docs/x.md', '../docs/x.md']) {
			const { failures } = withProbe(item(`[x](${href})`), ['docs/x.md']);
			expect(
				failures.map((f) => f.code),
				href
			).toEqual(['CHANGELOG-BAD-LINK']);
		}
	});

	it('names both readings it tried when neither resolves', () => {
		const { failures } = withProbe(item('[x](docs/specs/gone.md)'), []);
		expect(failures).toHaveLength(1);
		expect(failures[0].problem).toContain('crates/trusty-mpm/docs/specs/gone.md');
		expect(failures[0].problem).toContain('`docs/specs/gone.md` against the repository root');
	});

	it('fails the build on a relative link whose target is not in the repository', () => {
		const { failures } = withProbe(item('[gone](../../docs/specs/gone.md)'), []);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-BAD-LINK');
		expect(failures[0].problem).toContain('docs/specs/gone.md');
	});

	it('leaves an absolute URL alone', () => {
		const { releases, failures } = withProbe(
			item('closes [#4250](https://github.com/bobmatnyc/trusty-tools/issues/4250)'),
			[]
		);
		expect(failures).toEqual([]);
		expect(releases[0].categories[0].items[0].html).toContain(
			'href="https://github.com/bobmatnyc/trusty-tools/issues/4250"'
		);
	});

	it('leaves an http:// URL alone', () => {
		const { releases, failures } = withProbe(item('see [x](http://example.com/a)'), []);
		expect(failures).toEqual([]);
		expect(releases[0].categories[0].items[0].html).toContain('href="http://example.com/a"');
	});

	it('fails the build on a javascript: link, the same as any other bad link', () => {
		const { failures } = withProbe(item('[click me](javascript:alert(1))'), []);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-BAD-LINK');
		expect(failures[0].problem).toContain('javascript:');
	});

	it('fails the build on a file:// link', () => {
		const { failures } = withProbe(item('[passwd](file:///etc/passwd)'), []);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('CHANGELOG-BAD-LINK');
		expect(failures[0].problem).toContain('file:');
	});

	// `<path>` outside backticks parses as an unknown element and renders as
	// NOTHING. Eight of these sit in the corpus, and a changelog is history
	// this site must not edit — so the text is recovered, never dropped.
	it('recovers an unbackticked metavariable instead of publishing a gap', () => {
		const { releases, failures } = withProbe(item('run trusty-search index <path> now'), []);
		expect(failures).toEqual([]);
		expect(releases[0].categories[0].items[0].text).toBe('run trusty-search index <path> now');
	});

	it('recovers a metavariable nested inside another one', () => {
		const { releases } = withProbe(item('align identity to tm-<project>-<n> naming'), []);
		expect(releases[0].categories[0].items[0].text).toBe(
			'align identity to tm-<project>-<n> naming'
		);
	});

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
