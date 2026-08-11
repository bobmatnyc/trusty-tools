/**
 * Why: `docs/public-manifest.tsv` is a SECURITY BOUNDARY, not a navigation
 * convenience — `docs/` holds 460-odd markdown files that are mostly internal,
 * and publication is one-way and search-indexed. So the site ENUMERATES this
 * file and never walks the tree: there is no catch-all route, no directory
 * rule, and no fallback that could turn an unlisted path into a page. A page
 * absent from the manifest has no prerendered output and therefore no URL.
 *
 * What: a parser for the two-record TSV, preserving FILE ORDER as navigation
 * order (there is no sort key, deliberately — inserting a page must not
 * renumber its neighbours). It re-checks the structural invariants
 * `scripts/check_public_docs.sh` enforces in CI, because a build that trusts a
 * gate it does not run is a build that ships whatever a bypassed hook let
 * through.
 *
 * Test: `manifest.test.ts` — the clean parse, nav ordering, and one case per
 * failure code, including the boundary test that an unlisted source resolves to
 * no route.
 */

import { throwIfFailed, type DocFailure } from './errors';

/** One published page. `line` is its manifest line, for failure reporting. */
export interface DocPage {
	sectionId: string;
	/** Repo-relative markdown path, e.g. `docs/intro.md`. */
	source: string;
	/** Manifest route, e.g. `/` or `/getting-started/install`. */
	route: string;
	/** Route as a SvelteKit rest-parameter value: `''` for `/`. */
	slug: string;
	title: string;
	line: number;
}

export interface DocSection {
	id: string;
	title: string;
	line: number;
	pages: DocPage[];
}

export interface DocManifest {
	sections: DocSection[];
	/** Every page, in manifest order — this is also prev/next order. */
	pages: DocPage[];
	byRoute: Map<string, DocPage>;
	bySource: Map<string, DocPage>;
	bySlug: Map<string, DocPage>;
}

/** `/` → `''`, `/a/b` → `a/b`. The inverse of `slugToRoute`. */
export function routeToSlug(route: string): string {
	return route.replace(/^\/+/, '').replace(/\/+$/, '');
}

/** `''` → `/`, `a/b` → `/a/b`. */
export function slugToRoute(slug: string): string {
	return slug === '' ? '/' : `/${slug}`;
}

/** The site path a manifest route is served at. Route `/` is `/docs` itself. */
export function routeToHref(route: string): string {
	return route === '/' ? '/docs' : `/docs${route}`;
}

export interface ParseManifestOptions {
	/** Path reported in failures. */
	manifestPath?: string;
	/** Existence probe for `source` values. Omit to skip the MISSING-SOURCE pass. */
	sourceExists?: (source: string) => boolean;
}

/**
 * Why: one pass that both builds the nav tree and proves the boundary holds,
 * so no caller can obtain a parsed manifest that was never validated.
 * What: parses `text`, accumulating every structural violation, then throws a
 * single `DocBuildError` listing all of them.
 * Test: `manifest.test.ts`.
 */
export function parseManifest(text: string, options: ParseManifestOptions = {}): DocManifest {
	const file = options.manifestPath ?? 'docs/public-manifest.tsv';
	const failures: DocFailure[] = [];

	const sections: DocSection[] = [];
	const pages: DocPage[] = [];
	const byRoute = new Map<string, DocPage>();
	const bySource = new Map<string, DocPage>();
	const sectionIds = new Set<string>();
	let current: DocSection | undefined;

	text.split('\n').forEach((raw, index) => {
		const line = index + 1;
		const row = raw.replace(/\r$/, '');
		if (row.trim() === '' || row.startsWith('#')) return;
		const fields = row.split('\t');

		if (fields[0] === 'SECTION') {
			if (fields.length !== 3) {
				failures.push({
					code: 'BAD-RECORD',
					file,
					line,
					problem: `SECTION row has ${fields.length} tab-separated fields, expected 3`,
					remedy: 'write it as `SECTION\\t<id>\\t<title>`'
				});
				return;
			}
			const [, id, title] = fields;
			if (sectionIds.has(id)) {
				failures.push({
					code: 'DUP-SECTION',
					file,
					line,
					problem: `section id \`${id}\` is already declared`,
					remedy: 'give this section a different id, or merge it into the existing one'
				});
				return;
			}
			sectionIds.add(id);
			current = { id, title, line, pages: [] };
			sections.push(current);
			return;
		}

		if (fields[0] !== 'PAGE') {
			failures.push({
				code: 'BAD-RECORD',
				file,
				line,
				problem: `first field is \`${fields[0]}\`, which is neither SECTION nor PAGE`,
				remedy: 'use one of the two record types, or prefix the line with `#` to comment it out'
			});
			return;
		}

		if (fields.length !== 5) {
			failures.push({
				code: 'BAD-RECORD',
				file,
				line,
				problem: `PAGE row has ${fields.length} tab-separated fields, expected 5`,
				remedy: 'write it as `PAGE\\t<section-id>\\t<source>\\t<route>\\t<title>`'
			});
			return;
		}

		const [, sectionId, source, route, title] = fields;

		if (!current) {
			failures.push({
				code: 'ORPHAN-PAGE',
				file,
				line,
				problem: `PAGE \`${route}\` appears before any SECTION row`,
				remedy: 'add a SECTION row above it, or move the page under an existing section'
			});
			return;
		}

		if (sectionId !== current.id) {
			failures.push({
				code: 'SECTION-MISMATCH',
				file,
				line,
				problem: `PAGE names section \`${sectionId}\` but follows section \`${current.id}\``,
				remedy: `change the field to \`${current.id}\`, or move the row under the section it names`
			});
			return;
		}

		if (!route.startsWith('/')) {
			failures.push({
				code: 'BAD-ROUTE',
				file,
				line,
				problem: `route \`${route}\` does not start with \`/\``,
				remedy: `write it as \`/${route}\``
			});
			return;
		}

		if (!source.startsWith('docs/') || source.split('/').includes('..')) {
			failures.push({
				code: 'ESCAPES-DOCS',
				file,
				line,
				problem: `source \`${source}\` is not a path inside \`docs/\``,
				remedy: 'publish only files under `docs/`; move the file there first'
			});
			return;
		}

		const duplicateRoute = byRoute.get(route);
		if (duplicateRoute) {
			failures.push({
				code: 'DUP-ROUTE',
				file,
				line,
				problem: `route \`${route}\` is already claimed on line ${duplicateRoute.line} by \`${duplicateRoute.source}\``,
				remedy: 'give this page a distinct route; two sources cannot share one URL'
			});
			return;
		}

		const duplicateSource = bySource.get(source);
		if (duplicateSource) {
			failures.push({
				code: 'DUP-SOURCE',
				file,
				line,
				problem: `source \`${source}\` is already published at \`${duplicateSource.route}\` (line ${duplicateSource.line})`,
				remedy: 'publish each source once; link to the existing route instead'
			});
			return;
		}

		if (options.sourceExists && !options.sourceExists(source)) {
			failures.push({
				code: 'MISSING-SOURCE',
				file,
				line,
				problem: `source \`${source}\` does not exist`,
				remedy: "point the row at the file's new path, or delete the row if the page is gone"
			});
			return;
		}

		const page: DocPage = { sectionId, source, route, slug: routeToSlug(route), title, line };
		current.pages.push(page);
		pages.push(page);
		byRoute.set(route, page);
		bySource.set(source, page);
	});

	throwIfFailed(failures);

	const bySlug = new Map(pages.map((page) => [page.slug, page]));
	return { sections, pages, byRoute, bySource, bySlug };
}
