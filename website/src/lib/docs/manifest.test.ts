import { describe, expect, it } from 'vitest';

import { DocBuildError } from './errors';
import { parseManifest, routeToHref, routeToSlug, slugToRoute } from './manifest';

/**
 * Why: the manifest is the publication boundary, so its parser is the piece
 * that has to be provably strict. Two of these tests exist because the issue
 * names them: an unlisted page must be unreachable, and a manifest row whose
 * source is missing must stop the build.
 * What: the clean parse and nav ordering, then one case per failure code.
 * Test: this file.
 */

const CLEAN = [
	'# a comment',
	'',
	'SECTION\tone\tSection One',
	'PAGE\tone\tdocs/intro.md\t/\tIntroduction',
	'PAGE\tone\tdocs/a.md\t/a\tPage A',
	'SECTION\ttwo\tSection Two',
	'PAGE\ttwo\tdocs/b.md\t/b/c\tPage B'
].join('\n');

const failuresOf = (run: () => unknown) => {
	try {
		run();
	} catch (error) {
		if (error instanceof DocBuildError) return error.failures;
		throw error;
	}
	throw new Error('expected the parse to fail, but it succeeded');
};

describe('route helpers', () => {
	it('round-trips routes and slugs', () => {
		expect(routeToSlug('/')).toBe('');
		expect(routeToSlug('/a/b')).toBe('a/b');
		expect(slugToRoute('')).toBe('/');
		expect(slugToRoute('a/b')).toBe('/a/b');
	});

	it('serves the root route at /docs itself, not /docs/', () => {
		expect(routeToHref('/')).toBe('/docs');
		expect(routeToHref('/a/b')).toBe('/docs/a/b');
	});
});

describe('parseManifest', () => {
	it('preserves file order as nav order', () => {
		const manifest = parseManifest(CLEAN);
		expect(manifest.sections.map((s) => s.id)).toEqual(['one', 'two']);
		expect(manifest.sections[0].pages.map((p) => p.title)).toEqual(['Introduction', 'Page A']);
		expect(manifest.pages.map((p) => p.route)).toEqual(['/', '/a', '/b/c']);
	});

	it('indexes pages by route, source, and slug', () => {
		const manifest = parseManifest(CLEAN);
		expect(manifest.byRoute.get('/b/c')?.source).toBe('docs/b.md');
		expect(manifest.bySource.get('docs/a.md')?.route).toBe('/a');
		expect(manifest.bySlug.get('')?.title).toBe('Introduction');
	});

	// The boundary. Every lookup the site can perform is on this manifest, so a
	// source that is absent from it resolves to nothing anywhere.
	it('gives an unlisted source no route, no slug, and no page', () => {
		const manifest = parseManifest(CLEAN);
		expect(manifest.bySource.has('docs/adr/0029-msrv.md')).toBe(false);
		expect(manifest.pages.some((p) => p.source === 'docs/adr/0029-msrv.md')).toBe(false);
		expect(manifest.byRoute.has('/adr/0029-msrv')).toBe(false);
		expect(manifest.bySlug.has('adr/0029-msrv')).toBe(false);
	});

	it('fails when a listed source does not exist', () => {
		const failures = failuresOf(() =>
			parseManifest('SECTION\ta\tA\nPAGE\ta\tdocs/gone.md\t/gone\tGone', {
				sourceExists: () => false
			})
		);
		expect(failures).toHaveLength(1);
		expect(failures[0].code).toBe('MISSING-SOURCE');
		expect(failures[0].line).toBe(2);
		expect(failures[0].problem).toContain('docs/gone.md');
	});

	it('fails on a duplicate route, naming the line that claimed it first', () => {
		const failures = failuresOf(() =>
			parseManifest(
				['SECTION\ta\tA', 'PAGE\ta\tdocs/one.md\t/x\tOne', 'PAGE\ta\tdocs/two.md\t/x\tTwo'].join(
					'\n'
				)
			)
		);
		expect(failures[0].code).toBe('DUP-ROUTE');
		expect(failures[0].line).toBe(3);
		expect(failures[0].problem).toContain('line 2');
	});

	it('fails on a duplicate source', () => {
		const failures = failuresOf(() =>
			parseManifest(
				['SECTION\ta\tA', 'PAGE\ta\tdocs/one.md\t/x\tOne', 'PAGE\ta\tdocs/one.md\t/y\tAgain'].join(
					'\n'
				)
			)
		);
		expect(failures[0].code).toBe('DUP-SOURCE');
	});

	it('fails on a source that escapes docs/', () => {
		const failures = failuresOf(() =>
			parseManifest('SECTION\ta\tA\nPAGE\ta\t../CLAUDE.md\t/claude\tClaude')
		);
		expect(failures[0].code).toBe('ESCAPES-DOCS');
	});

	it('fails on a page before any section, a bad route, and a bad record', () => {
		expect(failuresOf(() => parseManifest('PAGE\ta\tdocs/x.md\t/x\tX'))[0].code).toBe(
			'ORPHAN-PAGE'
		);
		expect(failuresOf(() => parseManifest('SECTION\ta\tA\nPAGE\ta\tdocs/x.md\tx\tX'))[0].code).toBe(
			'BAD-ROUTE'
		);
		expect(failuresOf(() => parseManifest('WIDGET\ta\tA'))[0].code).toBe('BAD-RECORD');
		expect(failuresOf(() => parseManifest('SECTION\ta'))[0].code).toBe('BAD-RECORD');
	});

	it('fails on a duplicate section id and a mismatched section reference', () => {
		expect(failuresOf(() => parseManifest('SECTION\ta\tA\nSECTION\ta\tB'))[0].code).toBe(
			'DUP-SECTION'
		);
		expect(
			failuresOf(() => parseManifest('SECTION\ta\tA\nPAGE\tb\tdocs/x.md\t/x\tX'))[0].code
		).toBe('SECTION-MISMATCH');
	});

	it('reports every finding in one build, not just the first', () => {
		const failures = failuresOf(() =>
			parseManifest(
				['SECTION\ta\tA', 'PAGE\ta\tdocs/x.md\tx\tX', 'PAGE\ta\t../y.md\t/y\tY', 'WIDGET'].join(
					'\n'
				)
			)
		);
		expect(failures.map((f) => f.code)).toEqual(['BAD-ROUTE', 'ESCAPES-DOCS', 'BAD-RECORD']);
	});
});
