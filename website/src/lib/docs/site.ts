/**
 * Why: the whole documentation site is one build-time computation — read the
 * manifest, read the 27 sources, cross-validate every link, emit HTML. Doing it
 * once and memoising is not an optimisation: `other.md#section` cannot be
 * checked until every page's headings are known, so the pages CANNOT be built
 * independently, and a per-route build would re-read the corpus 27 times to
 * reach the same answer.
 *
 * What: `buildDocSite()` — the single entry point every `+page.server.ts` and
 * `+layout.server.ts` in `src/routes/docs/` calls. It runs in Node at build
 * time only; its output is prerendered to static HTML and a JSON payload, so
 * nothing here executes on a request and the published page opens no
 * connection to anything.
 *
 * The boundary lives here too: `pages` is derived from the manifest and from
 * nothing else. There is no tree walk, no glob, and no fallback that reads a
 * path the manifest did not name, which is why an unlisted `docs/` file has no
 * prerendered output and therefore no URL.
 *
 * Test: `site.test.ts` — the real 27-page corpus builds clean, an unlisted
 * source is absent from every lookup, and a broken link fails the build.
 */

import { DocBuildError, throwIfFailed, type DocFailure } from './errors';
import { parseManifest, routeToHref, type DocManifest, type DocPage } from './manifest';
import { parsePage, renderPage, type TocEntry } from './render';
import type { LinkClass } from './links';
import { blobUrl, findRepoRoot, probeRepoEntry, readRepoFile, resolveCommitSha } from './repo';

export const MANIFEST_PATH = 'docs/public-manifest.tsv';

export interface DocNavPage {
	href: string;
	title: string;
}

export interface DocNavSection {
	id: string;
	title: string;
	pages: DocNavPage[];
}

export interface DocLink {
	href: string;
	title: string;
}

export interface BuiltPage {
	slug: string;
	route: string;
	href: string;
	title: string;
	sectionTitle: string;
	/** Repo-relative markdown path this page was rendered from. */
	source: string;
	/** Commit-pinned permalink to that source. */
	sourceUrl: string;
	html: string;
	toc: TocEntry[];
	prev?: DocLink;
	next?: DocLink;
}

export interface DocSite {
	nav: DocNavSection[];
	pages: BuiltPage[];
	bySlug: Map<string, BuiltPage>;
	commitSha: string;
	/** Link totals across the corpus, by class. Build-log evidence. */
	linkCounts: Record<LinkClass, number>;
}

let cached: DocSite | undefined;

/** Builds the site once per process. Exposed for tests that mutate fixtures. */
export function clearDocSiteCache(): void {
	cached = undefined;
}

/**
 * Why/What/Test: see the module header.
 * @param repoRootOverride test seam; production always discovers the root.
 */
export function buildDocSite(repoRootOverride?: string): DocSite {
	if (cached && repoRootOverride === undefined) return cached;

	const repoRoot = repoRootOverride ?? findRepoRoot();
	const manifest = readManifest(repoRoot);
	const commitSha = resolveCommitSha(repoRoot);

	// Phase 1: parse every page, so cross-page anchors have something to check
	// against before any link is rewritten.
	const parsed = new Map(
		manifest.pages.map((page) => [page.source, parsePage(readRepoFile(repoRoot, page.source))])
	);
	const headingIds = (source: string) => parsed.get(source)?.headingIds ?? new Set<string>();

	// Phase 2: rewrite and stringify, accumulating every failure in the corpus.
	const failures: DocFailure[] = [];
	const linkCounts: Record<LinkClass, number> = {
		external: 0,
		anchor: 0,
		site: 0,
		'repo-file': 0,
		'repo-dir': 0
	};

	const pages = manifest.pages.map((page, index) => {
		const parsedPage = parsed.get(page.source)!;
		const rendered = renderPage(parsedPage, {
			fromSource: page.source,
			bySource: manifest.bySource,
			probe: (relative) => probeRepoEntry(repoRoot, relative),
			commitSha,
			headingIds
		});
		failures.push(...rendered.failures);
		for (const key of Object.keys(linkCounts) as LinkClass[]) {
			linkCounts[key] += rendered.counts[key];
		}
		return toBuiltPage(page, index, manifest, rendered.html, parsedPage.toc, commitSha);
	});

	throwIfFailed(failures);

	const site: DocSite = {
		nav: buildNav(manifest),
		pages,
		bySlug: new Map(pages.map((page) => [page.slug, page])),
		commitSha,
		linkCounts
	};

	if (repoRootOverride === undefined) cached = site;
	return site;
}

/**
 * Why: `adapter-vercel` always provisions a catchall serverless function, even
 * when every route is prerendered. A request can only reach it by asking for a
 * path with no prerendered file — which, for `/docs/*`, means a path the
 * manifest never named. That function's bundle contains no repository, so the
 * honest answer is 404, not the 500 an unreadable `../docs` would otherwise
 * produce.
 *
 * What: `buildDocSite()`, except that a MISSING REPOSITORY yields `undefined`.
 * Every other finding — a broken link, a missing source, a route collision —
 * still throws, because those must stop a build rather than be swallowed.
 * Test: `site.test.ts` (`returns undefined when there is no repository to read`
 * and `still throws a real gate failure`).
 */
export function buildDocSiteIfAvailable(): DocSite | undefined {
	try {
		return buildDocSite();
	} catch (error) {
		if (
			error instanceof DocBuildError &&
			error.failures.every((failure) => failure.code === 'NO-REPO-ROOT')
		) {
			return undefined;
		}
		throw error;
	}
}

function readManifest(repoRoot: string): DocManifest {
	return parseManifest(readRepoFile(repoRoot, MANIFEST_PATH), {
		manifestPath: MANIFEST_PATH,
		sourceExists: (source) => probeRepoEntry(repoRoot, source) === 'file'
	});
}

function toBuiltPage(
	page: DocPage,
	index: number,
	manifest: DocManifest,
	html: string,
	toc: TocEntry[],
	commitSha: string
): BuiltPage {
	const section = manifest.sections.find((candidate) => candidate.id === page.sectionId);
	const neighbour = (offset: number): DocLink | undefined => {
		const other = manifest.pages[index + offset];
		return other ? { href: routeToHref(other.route), title: other.title } : undefined;
	};
	return {
		slug: page.slug,
		route: page.route,
		href: routeToHref(page.route),
		title: page.title,
		sectionTitle: section?.title ?? '',
		source: page.source,
		sourceUrl: blobUrl(commitSha, page.source),
		html,
		toc,
		prev: neighbour(-1),
		next: neighbour(1)
	};
}

function buildNav(manifest: DocManifest): DocNavSection[] {
	return manifest.sections.map((section) => ({
		id: section.id,
		title: section.title,
		pages: section.pages.map((page) => ({ href: routeToHref(page.route), title: page.title }))
	}));
}
