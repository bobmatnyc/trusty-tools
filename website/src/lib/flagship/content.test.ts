/**
 * Why: the flagship pages are data now, so the failures worth pinning are the
 * ones a build could otherwise ship silently — a page rendering as an empty
 * frame, an include directive that resolved to nothing, or a link into a route
 * that stopped existing. The Cost savings case is asserted by name because it
 * is the whole point of the include mechanism: one `docs/` file publishing at
 * `/docs` and inside `/tools/trusty-mpm` at once.
 *
 * What: one pass over the real corpus in this repository, then a temp-repo
 * fixture per gate — the same shape `../docs/site.test.ts` uses, including its
 * `TRUSTY_DOCS_COMMIT_SHA` seam so a fixture root needs no git history.
 *
 * Test: this file.
 */

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';

import { DocBuildError } from '../docs/errors';
import { clearDocSiteCache } from '../docs/site';
import { TOOLS } from '../tools';
import { buildFlagshipContent, CONTENT_DIR, clearFlagshipContentCache } from './content';

const SHA = 'e'.repeat(40);

/** The six slugs that render from markdown — trusty-audit stays Svelte. */
const MARKDOWN_SLUGS = [
	'trusty-analyze',
	'trusty-git-analytics',
	'trusty-memory',
	'trusty-mpm',
	'trusty-review',
	'trusty-search'
];

/** Enough manifest for the doc site to build, so link resolution is real. */
const MANIFEST = ['SECTION\tg\tGuides', 'PAGE\tg\tdocs/intro.md\t/\tIntroduction', ''].join('\n');

const scratch: string[] = [];

/** A throwaway repository root carrying only what a fixture case needs. */
function fixtureRoot(files: Record<string, string>): string {
	const root = mkdtempSync(path.join(tmpdir(), 'trusty-flagship-'));
	scratch.push(root);
	const write = (relative: string, contents: string) => {
		const absolute = path.join(root, relative);
		mkdirSync(path.dirname(absolute), { recursive: true });
		writeFileSync(absolute, contents);
	};
	write('docs/public-manifest.tsv', MANIFEST);
	write('docs/intro.md', '# Introduction\n');
	mkdirSync(path.join(root, CONTENT_DIR), { recursive: true });
	for (const [relative, contents] of Object.entries(files)) write(relative, contents);
	return root;
}

/** One page's markdown, at the path the loader enumerates. */
const page = (slug: string, markdown: string) => ({ [`${CONTENT_DIR}/${slug}.md`]: markdown });

/** The codes one failed build reported, sorted, for a stable assertion. */
function codesFrom(files: Record<string, string>): string[] {
	process.env.TRUSTY_DOCS_COMMIT_SHA = SHA;
	try {
		buildFlagshipContent(fixtureRoot(files));
	} catch (error) {
		if (error instanceof DocBuildError) return error.failures.map((f) => f.code).sort();
		throw error;
	}
	throw new Error('expected the build to fail, but it succeeded');
}

afterEach(() => {
	delete process.env.TRUSTY_DOCS_COMMIT_SHA;
	clearFlagshipContentCache();
	clearDocSiteCache();
	for (const dir of scratch.splice(0)) rmSync(dir, { recursive: true, force: true });
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

	it('gives every markdown slug a tool record, and leaves trusty-audit alone', () => {
		const slugs = new Set(TOOLS.map((tool) => tool.slug));
		for (const slug of MARKDOWN_SLUGS) expect(slugs.has(slug), slug).toBe(true);
		expect(built.has('trusty-audit')).toBe(false);
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
	 * `/docs` in its own right.
	 */
	it('carries the Cost savings section into the trusty-mpm page from docs/', () => {
		const mpm = built.get('trusty-mpm');
		expect(mpm?.sources).toContain('docs/trusty-mpm/statusline-savings.md');
		expect(mpm?.html).toContain('>Cost savings</h2>');
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

describe('build gates', () => {
	it('fails on a root-relative link naming no route the site builds', () => {
		expect(codesFrom(page('trusty-mpm', '## A\n\n[nope](/not-a-route)\n'))).toEqual([
			'UNKNOWN-SITE-ROUTE'
		]);
	});

	it('fails on an include directive naming a file that does not exist', () => {
		expect(codesFrom(page('trusty-mpm', '## A\n\n<!-- include: docs/nope.md -->\n'))).toEqual([
			'BAD-INCLUDE'
		]);
	});

	it('fails on an include directive escaping the repository', () => {
		expect(codesFrom(page('trusty-mpm', '<!-- include: ../secrets.md -->\n'))).toEqual([
			'BAD-INCLUDE'
		]);
	});

	// Two sources concatenated into one document: the second `id` is
	// unreachable and its `#anchor` silently lands on the first.
	it('fails when two of a page’s sources produce the same heading id', () => {
		expect(
			codesFrom({
				'docs/savings.md': '# Savings\n\n## Cost savings\n\nTheirs.\n',
				...page('trusty-mpm', '## Cost savings\n\nOurs.\n\n<!-- include: docs/savings.md -->\n')
			})
		).toEqual(['DUP-HEADING-ID']);
	});

	it('fails on a markdown file naming no flagship', () => {
		expect(codesFrom(page('trusty-nonesuch', '## A\n'))).toEqual(['UNKNOWN-TOOL']);
	});
});
