import { beforeAll, describe, expect, it } from 'vitest';

import { RELEASED_FLAGSHIPS } from '../site';
import {
	buildChangelogSite,
	DETAILED_RELEASES,
	stripItems,
	whatsNewSections,
	type ChangelogSite
} from './site';

/**
 * Why: these assertions read the REAL six-crate changelog corpus, which grows
 * with every release — the grammar deviations they catch live in the files and
 * in no fixture. They are separated from `site.test.ts` because that cost is
 * not a unit-test cost: the corpus parse blew vitest's DEFAULT 10 s
 * `hookTimeout` in `beforeAll` on PR #8272, a Rust-only fix, and every release
 * PR touches the same trigger paths. The gate still runs — as its own CI check
 * (`Changelog corpus`), on the changes that can actually move the corpus.
 * What: one shared build of the corpus per file, then the same assertions that
 * lived in `site.test.ts`'s corpus blocks, unchanged.
 * Test: this file; the fixture-driven gates stay in `site.test.ts`.
 */

// One build per file rather than one per block, through the PRODUCTION entry
// point with no root override. `buildChangelogSite` memoises only the
// no-override call (`site.ts`'s `cached`), so passing an explicit REPO_ROOT —
// what every block used to do — re-parsed the 11k-line corpus six times over,
// ~21 s each. Nothing below mutates either value. The hook's budget is
// `CORPUS_TIMEOUT_MS` in `vite.config.ts`, the one place the measurement and
// its date are recorded.
let site: ChangelogSite;
let whatsNew: ReturnType<typeof whatsNewSections>;

beforeAll(() => {
	site = buildChangelogSite();
	whatsNew = whatsNewSections();
});

describe('the real flagship-crate corpus', () => {
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
		// A non-semver label.
		expect(versions('trusty-mpm')).toContain('consolidation');
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
	 * Only non-flagship crates such as `trusty-common` — never carded or
	 * paged — stay out of this surface. `trusty-audit` and `tga` moved to
	 * their own site in #8507 and, with them, out of `RELEASED_FLAGSHIPS`.
	 */
	it('includes every released flagship, and only non-flagship crates stay out', () => {
		expect(site.crates.map((c) => c.name)).not.toContain('trusty-common');
		expect(site.crates).toHaveLength(RELEASED_FLAGSHIPS.length);
	});
});

describe('the /whats-new projection', () => {
	it('splits every release into exactly one of detailed or earlier', () => {
		for (const crate of whatsNew.crates) {
			expect(crate.detailed.length, crate.name).toBeGreaterThan(0);
			expect(crate.detailed.length).toBeLessThanOrEqual(DETAILED_RELEASES);
			expect(crate.detailed.length + crate.earlier.length).toBe(crate.releaseCount);

			const detailed = new Set(crate.detailed.map((release) => release.version));
			for (const summary of crate.earlier) expect(detailed.has(summary.version)).toBe(false);
		}
	});

	it('ships no item prose for a summarised release', () => {
		const crate = whatsNew.crates.find((c) => c.name === 'trusty-search')!;
		expect(crate.earlier.length).toBeGreaterThan(100);
		for (const summary of crate.earlier) {
			expect(Object.keys(summary).sort()).toEqual(['date', 'title', 'version']);
		}
	});

	it('keeps the newest release detailed, so the page opens on what just shipped', () => {
		for (const [index, crate] of whatsNew.crates.entries()) {
			expect(crate.detailed[0].version).toBe(site.crates[index].latest.version);
		}
	});
});

describe('the landing-page strip over the real corpus', () => {
	it('produces a non-empty strip for every real flagship crate', () => {
		for (const crate of site.crates) {
			const lines = stripItems(crate.latest);
			expect(lines.length, crate.name).toBeGreaterThan(0);
			expect(lines.length).toBeLessThanOrEqual(3);
			for (const line of lines) expect(line.text.trim(), crate.name).not.toBe('');
		}
	});
});
