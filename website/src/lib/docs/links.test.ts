import { describe, expect, it } from 'vitest';

import { resolveLink, type LinkContext } from './links';
import type { DocPage } from './manifest';

/**
 * Why: link classification is where a published page either stays honest or
 * quietly turns into a list of dead references to internal documents. Each of
 * the five classes in the module header gets a test, including both build-
 * failing ones.
 * What: a fixture repository — two published pages, one unpublished `docs/`
 * page, one directory, one file outside `docs/` — driven entirely through the
 * injected `probe` and `headingIds`, so no filesystem is touched.
 * Test: this file.
 */

const SHA = 'a'.repeat(40);
const BLOB = `https://github.com/bobmatnyc/trusty-tools/blob/${SHA}`;
const TREE = `https://github.com/bobmatnyc/trusty-tools/tree/${SHA}`;

const page = (source: string, route: string): DocPage => ({
	sectionId: 's',
	source,
	route,
	slug: route.replace(/^\//, ''),
	title: source,
	line: 1
});

const PUBLISHED = new Map<string, DocPage>([
	['docs/intro.md', page('docs/intro.md', '/')],
	['docs/guides/quickstart.md', page('docs/guides/quickstart.md', '/guides/quickstart')]
]);

const TREE_ENTRIES: Record<string, 'file' | 'dir'> = {
	'docs/intro.md': 'file',
	'docs/guides/quickstart.md': 'file',
	'docs/specs/spec-foo.md': 'file',
	'docs/adr': 'dir',
	'README.md': 'file',
	'crates/trusty-mpm/docs/WHAT-IS.md': 'file'
};

const HEADINGS: Record<string, string[]> = {
	'docs/intro.md': ['why-this-exists'],
	'docs/guides/quickstart.md': ['first-run']
};

const ctx = (fromSource: string): LinkContext => ({
	fromSource,
	bySource: PUBLISHED,
	probe: (relative) => TREE_ENTRIES[relative] ?? 'missing',
	commitSha: SHA,
	headingIds: (source) => new Set(HEADINGS[source] ?? []),
	line: 7
});

const resolve = (href: string, from = 'docs/guides/quickstart.md') => resolveLink(href, ctx(from));

const link = (href: string, from?: string) => {
	const outcome = resolve(href, from);
	if ('failure' in outcome) throw new Error(`expected a link, got ${outcome.failure.code}`);
	return outcome.link;
};

const failure = (href: string, from?: string) => {
	const outcome = resolve(href, from);
	if ('link' in outcome) throw new Error(`expected a failure, got ${outcome.link.href}`);
	return outcome.failure;
};

describe('class 1 — target is on the manifest', () => {
	it('rewrites it to its site route', () => {
		expect(link('../intro.md', 'docs/guides/quickstart.md')).toEqual({
			class: 'site',
			href: '/docs',
			external: false
		});
		expect(link('./guides/quickstart.md', 'docs/intro.md').href).toBe('/docs/guides/quickstart');
	});
});

describe('class 2 — target exists in the repository but is not published', () => {
	it('pins an unpublished docs/ file to a commit permalink', () => {
		expect(link('../specs/spec-foo.md')).toEqual({
			class: 'repo-file',
			href: `${BLOB}/docs/specs/spec-foo.md`,
			external: true
		});
	});

	it('pins a directory to a commit-pinned tree link', () => {
		expect(link('../adr')).toEqual({ class: 'repo-dir', href: `${TREE}/docs/adr`, external: true });
	});

	it('treats a target outside docs/ exactly the same way', () => {
		expect(link('../../README.md').href).toBe(`${BLOB}/README.md`);
		expect(link('../../crates/trusty-mpm/docs/WHAT-IS.md').href).toBe(
			`${BLOB}/crates/trusty-mpm/docs/WHAT-IS.md`
		);
	});

	it('never emits a blob/main link, which would silently retarget', () => {
		expect(link('../specs/spec-foo.md').href).not.toContain('/blob/main/');
		expect(link('../specs/spec-foo.md').href).toContain(`/blob/${SHA}/`);
	});

	it('carries a fragment onto the permalink', () => {
		expect(link('../../README.md#installation').href).toBe(`${BLOB}/README.md#installation`);
	});
});

describe('class 3 — anchor-only links', () => {
	it('leaves a resolvable same-page anchor alone', () => {
		expect(link('#first-run')).toEqual({ class: 'anchor', href: '#first-run', external: false });
	});

	it('fails the build on an anchor with no matching heading', () => {
		expect(failure('#no-such-heading').code).toBe('BROKEN-ANCHOR');
	});
});

describe('class 4 — cross-page anchors', () => {
	it('resolves against the TARGET page headings', () => {
		expect(link('../intro.md#why-this-exists').href).toBe('/docs#why-this-exists');
	});

	it('fails when the target page has no such heading', () => {
		const found = failure('../intro.md#first-run');
		expect(found.code).toBe('BROKEN-ANCHOR');
		expect(found.problem).toContain('docs/intro.md');
	});

	it('does not check anchors on unpublished targets, whose headings are GitHub-side', () => {
		expect(link('../specs/spec-foo.md#anything').href).toBe(
			`${BLOB}/docs/specs/spec-foo.md#anything`
		);
	});
});

describe('class 5 — links that stop the build', () => {
	it('fails on a target that does not exist', () => {
		const found = failure('./deleted-page.md');
		expect(found.code).toBe('BROKEN-LINK');
		expect(found.file).toBe('docs/guides/quickstart.md');
		expect(found.line).toBe(7);
		expect(found.problem).toContain('docs/guides/deleted-page.md');
		expect(found.remedy).toContain('delete the link and keep its text');
	});

	it('fails on a target outside the repository', () => {
		expect(failure('../../../elsewhere/notes.md').code).toBe('ESCAPES-REPO');
	});

	it('fails on a root-relative path, which means different things on GitHub and here', () => {
		expect(failure('/docs/intro.md').code).toBe('ABSOLUTE-PATH-LINK');
	});

	it('fails on an empty destination', () => {
		expect(failure('').code).toBe('EMPTY-LINK');
	});
});

describe('external links', () => {
	it('passes http(s) and mailto through untouched', () => {
		expect(link('https://example.com/x')).toEqual({
			class: 'external',
			href: 'https://example.com/x',
			external: true
		});
		expect(link('mailto:someone@example.com').class).toBe('external');
	});
});

describe('anchor normalisation', () => {
	it('lower-cases the fragment, as GitHub does', () => {
		expect(link('#First-Run').href).toBe('#First-Run');
	});

	it('percent-decodes a target path', () => {
		expect(link('../specs/spec-foo.md').href).toContain('spec-foo.md');
		expect(link('..%2Fspecs/spec-foo.md').href).toContain('docs/specs/spec-foo.md');
	});
});
