/**
 * Why: the flagship pages are data, so the failures worth pinning against the
 * REAL corpus are the ones a build could otherwise ship silently — a page
 * rendering as an empty frame, an include directive that resolved to nothing,
 * or a link into a route that stopped existing. This reads
 * `website/src/content/**` and `docs/**`, so it is a CORPUS suite: a content or
 * docs change pays for this check and not the website code suite (#8272).
 * What: one `buildFlagshipContent()` pass over the real repository. The fixture
 * gates stay in `content.test.ts`.
 * Test: this file.
 */

import { afterEach, beforeAll, describe, expect, it } from 'vitest';

import { findRepoRoot, readRepoFile } from '../docs/repo';
import { clearDocSiteCache } from '../docs/site';
import { TOOLS } from '../tools';
import { buildFlagshipContent, clearFlagshipContentCache } from './content';

/** The five slugs that render from markdown. */
const MARKDOWN_SLUGS = [
	'trusty-analyze',
	'trusty-memory',
	'trusty-mpm',
	'trusty-review',
	'trusty-search'
];

afterEach(() => {
	clearFlagshipContentCache();
	clearDocSiteCache();
});

describe('the real flagship corpus', () => {
	let built: Map<string, { sources: string[]; html: string }>;

	beforeAll(() => {
		clearFlagshipContentCache();
		clearDocSiteCache();
		built = buildFlagshipContent();
	});

	it('renders one page per markdown source, and only those', () => {
		expect([...built.keys()].sort()).toEqual(MARKDOWN_SLUGS);
	});

	it('gives every markdown slug a tool record', () => {
		const slugs = new Set(TOOLS.map((tool) => tool.slug));
		for (const slug of MARKDOWN_SLUGS) expect(slugs.has(slug), slug).toBe(true);
	});

	it('renders real prose, not an empty frame', () => {
		for (const [slug, content] of built) {
			expect(content.html.length, slug).toBeGreaterThan(1000);
			expect(content.html, slug).toContain('<h2');
		}
	});

	/**
	 * The include mechanism, end to end: the heading comes from a `docs/` file
	 * this page never names in its own prose, and that file is published at
	 * `/docs` in its own right. The expected heading text is read from that
	 * source doc itself (renamed "Cost savings" -> "Token savings" by #7179)
	 * rather than hardcoded a second time here.
	 */
	it('carries the Token savings section into the trusty-mpm page from docs/', () => {
		const source = 'docs/trusty-mpm/statusline-savings.md';
		const heading = readRepoFile(findRepoRoot(), source).match(/^##\s+(.+)$/m)?.[1];
		expect(heading, `${source} has no level-2 heading`).toBeDefined();

		const mpm = built.get('trusty-mpm');
		expect(mpm?.sources).toContain(source);
		expect(mpm?.html).toContain(`>${heading}</h2>`);
	});

	/** The included file's own `/docs` page title must not survive the include. */
	it('drops the included file’s h1, leaving the hero as the only page title', () => {
		expect(built.get('trusty-mpm')?.html).not.toContain('<h1');
	});

	it('rewrites root-relative links to real site routes', () => {
		expect(built.get('trusty-mpm')?.html).toContain('href="/claude-mpm-migration"');
		expect(built.get('trusty-review')?.html).toContain('href="/docs/guides/audit-instructions"');
	});
});
