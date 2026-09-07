/**
 * Why: the six flagship pages under `/tools/` used to carry their prose as
 * Svelte markup, so changing a sentence meant editing a `.svelte` file and
 * every page drifted its own way with `<ul>`/`<li>`/`<span>` bullet scaffolding
 * repeated per item. Owner ruling (2026-09-07): a documentation change must not
 * require Svelte coding. The prose therefore lives in markdown and the page
 * component is a shell around it.
 *
 * What: reads one markdown file per flagship slug from `website/src/content/
 * tools/`, resolves any `<!-- include: <repo-relative path> -->` directive it
 * carries, and renders every piece through the SAME remark/rehype pipeline the
 * documentation reader uses (`$lib/docs/render`). One pipeline, so a table, a
 * fenced block, or an angle-bracket metavariable behaves identically on a
 * flagship page and on `/docs`, and a broken link fails the build on both.
 *
 * The include directive is what makes a `docs/` file publishable on two
 * surfaces at once: `docs/trusty-mpm/statusline-savings.md` is a `/docs` page
 * in its own right AND the Cost savings section of `/tools/trusty-mpm`, from
 * one source. An included file's leading `<h1>` is dropped — that heading is
 * its `/docs` page title, and the flagship page already has one in its hero.
 *
 * Two differences from a `docs/` page, both deliberate:
 *
 *   - Root-relative links are ALLOWED, against the `siteHrefs` allowlist below.
 *     A `docs/` source is read on GitHub as often as on the site, so `/foo`
 *     means two things there; this copy is only ever read on the site.
 *   - Heading ids are checked across ALL of a page's sources at once, and a
 *     collision fails the build, because the pieces are concatenated into one
 *     document and two `#cost-savings` anchors would silently shadow.
 *
 * Test: `content.test.ts` — the real six-page corpus builds, trusty-mpm carries
 * the included Cost savings heading, an unknown site route fails, and a
 * duplicate heading id across two sources fails.
 */

import { readdirSync } from 'node:fs';
import path from 'node:path';

import { throwIfFailed, type DocFailure } from '$lib/docs/errors';
import { parsePage, renderPage, type ParsedPage } from '$lib/docs/render';
import { findRepoRoot, probeRepoEntry, readRepoFile } from '$lib/docs/repo';
import { buildDocSite, buildDocSiteIfAvailable, type DocSite } from '$lib/docs/site';
import { NAV_LINKS } from '$lib/site';
import { TOOLS } from '$lib/tools';

/** Repo-relative directory holding one markdown file per markdown-driven page. */
export const CONTENT_DIR = 'website/src/content/tools';

/**
 * A line that is nothing but this directive pulls another repository file in at
 * that point. Anchored and whole-line so a directive quoted inside prose or a
 * fenced block is left alone.
 */
const INCLUDE = /^<!--\s*include:\s*([^\s]+)\s*-->$/;

/**
 * Anchor ids `ToolPage.svelte` renders around the markdown, so copy may link
 * `#install` without the anchor check failing on a heading the markdown does
 * not contain.
 */
const LAYOUT_ANCHORS: readonly string[] = ['install'];

/**
 * Site routes no build-time table enumerates: hand-authored pages with no
 * `TOOLS` record. `tests/build-smoke.test.ts` keeps its own list for the same
 * reason — these are routes, not data.
 */
const EXTRA_ROUTES: readonly string[] = ['/', '/install', '/tools/trusty-git-analytics/audit'];

export interface FlagshipContent {
	/** Route segment: the page is served at `/tools/<slug>`. */
	slug: string;
	/** Repo-relative sources this page was rendered from, in render order. */
	sources: string[];
	/** The page body, ready for `{@html}`. */
	html: string;
}

/** One markdown source and its parsed tree, before link rewriting. */
interface Unit {
	source: string;
	parsed: ParsedPage;
}

let cached: Map<string, FlagshipContent> | undefined;

/** Builds once per process. Exposed for tests that point at a fixture root. */
export function clearFlagshipContentCache(): void {
	cached = undefined;
}

/**
 * Why/What/Test: see the module header.
 * @param repoRootOverride test seam; production always discovers the root.
 */
export function buildFlagshipContent(repoRootOverride?: string): Map<string, FlagshipContent> {
	if (cached && repoRootOverride === undefined) return cached;

	const repoRoot = repoRootOverride ?? findRepoRoot();
	const docSite = buildDocSite(repoRootOverride);
	const hrefs = siteHrefs(docSite);
	const failures: DocFailure[] = [];
	const built = new Map<string, FlagshipContent>();

	for (const slug of contentSlugs(repoRoot, failures)) {
		const source = `${CONTENT_DIR}/${slug}.md`;
		const units = readUnits(repoRoot, source, failures);
		const ownSources = new Set(units.map((unit) => unit.source));
		const pageIds = collectHeadingIds(units, failures);

		const html = units
			.map((unit) => {
				const rendered = renderPage(unit.parsed, {
					fromSource: unit.source,
					bySource: docSite.bySource,
					probe: (relative) => probeRepoEntry(repoRoot, relative),
					commitSha: docSite.commitSha,
					headingIds: (from) =>
						ownSources.has(from) ? pageIds : (docSite.headingIdsBySource.get(from) ?? new Set()),
					siteHrefs: hrefs
				});
				failures.push(...rendered.failures);
				return rendered.html;
			})
			.join('\n');

		built.set(slug, { slug, sources: units.map((unit) => unit.source), html });
	}

	throwIfFailed(failures);

	if (repoRootOverride === undefined) cached = built;
	return built;
}

/**
 * Why: `adapter-vercel` provisions a catchall function whose bundle carries no
 * repository, exactly as it does for `/docs` — so the same "no repository means
 * 404, every other finding still fails the build" rule applies here.
 * What: `buildFlagshipContent()`, except that a missing repository yields
 * `undefined`.
 * Test: `../docs/site.test.ts` covers the underlying probe; this wrapper adds
 * no branch of its own.
 */
export function buildFlagshipContentIfAvailable(): Map<string, FlagshipContent> | undefined {
	if (buildDocSiteIfAvailable() === undefined) return undefined;
	return buildFlagshipContent();
}

/**
 * The slugs served from markdown, from the directory listing rather than a
 * second table that could disagree with it. A file naming no flagship fails the
 * build; a flagship with no file is hand-authored Svelte and simply absent —
 * `trusty-audit` is the one such page, because its copy embeds a live
 * `CopyButton` component that markdown cannot express.
 */
function contentSlugs(repoRoot: string, failures: DocFailure[]): string[] {
	const known = new Set(TOOLS.map((tool) => tool.slug));
	const slugs: string[] = [];

	for (const entry of readdirSync(path.join(repoRoot, CONTENT_DIR)).sort()) {
		if (!entry.endsWith('.md')) continue;
		const slug = entry.slice(0, -'.md'.length);
		if (!known.has(slug)) {
			failures.push({
				code: 'UNKNOWN-TOOL',
				file: `${CONTENT_DIR}/${entry}`,
				problem: `\`${slug}\` names no entry in \`$lib/tools\`, so nothing serves this file`,
				remedy: 'rename the file to a flagship slug, or add the tool record it names'
			});
			continue;
		}
		slugs.push(slug);
	}

	return slugs;
}

/**
 * Reads one page's markdown and every file its include directives name, in
 * order. Includes are one level deep: a directive inside an included file is
 * left as literal text, which the `RAW-HTML` gate does not see because a
 * comment is not an element.
 */
function readUnits(repoRoot: string, source: string, failures: DocFailure[]): Unit[] {
	const units: Unit[] = [];
	const lines = readRepoFile(repoRoot, source).split('\n');
	let held: string[] = [];

	const flush = () => {
		if (held.join('').trim() === '') return;
		units.push({ source, parsed: parsePage(held.join('\n')) });
		held = [];
	};

	lines.forEach((line, index) => {
		const match = INCLUDE.exec(line.trim());
		if (!match) {
			held.push(line);
			return;
		}
		flush();
		const included = includedUnit(repoRoot, source, match[1], index + 1, failures);
		if (included) units.push(included);
	});
	flush();

	return units;
}

/** One included file, with its `/docs` page title stripped. */
function includedUnit(
	repoRoot: string,
	from: string,
	target: string,
	line: number,
	failures: DocFailure[]
): Unit | undefined {
	const fail = (problem: string, remedy: string) => {
		failures.push({ code: 'BAD-INCLUDE', file: from, line, problem, remedy });
		return undefined;
	};

	const normalised = path.posix.normalize(target);
	if (normalised.startsWith('..') || path.posix.isAbsolute(normalised)) {
		return fail(
			`\`${target}\` is not a path inside the repository`,
			'name the file repo-relative, as `docs/<crate>/<file>.md`'
		);
	}
	if (probeRepoEntry(repoRoot, normalised) !== 'file') {
		return fail(
			`\`${normalised}\` does not exist`,
			"point the directive at the file's current path, or remove it"
		);
	}

	const parsed = parsePage(readRepoFile(repoRoot, normalised));
	dropLeadingTitle(parsed);
	return { source: normalised, parsed };
}

/**
 * Removes an included file's opening `<h1>`. That heading titles its own
 * `/docs` page; on a flagship page the hero already carries the title, and a
 * second `<h1>` mid-document is both wrong for a screen reader and visually a
 * page break.
 */
function dropLeadingTitle(parsed: ParsedPage): void {
	const at = parsed.tree.children.findIndex((child) => child.type === 'element');
	if (at === -1) return;
	const first = parsed.tree.children[at];
	if (first.type !== 'element' || first.tagName !== 'h1') return;
	if (typeof first.properties?.id === 'string') parsed.headingIds.delete(first.properties.id);
	parsed.tree.children.splice(at, 1);
}

/**
 * Every heading id across one page's sources, plus the layout's own anchors.
 * A collision fails the build: the sources are concatenated into one document,
 * where the second `id` is unreachable and its `#anchor` silently lands on the
 * first.
 */
function collectHeadingIds(units: Unit[], failures: DocFailure[]): Set<string> {
	const seen = new Map<string, string>();
	const ids = new Set<string>(LAYOUT_ANCHORS);

	for (const unit of units) {
		for (const id of unit.parsed.headingIds) {
			const owner = seen.get(id);
			if (owner !== undefined && owner !== unit.source) {
				failures.push({
					code: 'DUP-HEADING-ID',
					file: unit.source,
					problem: `heading id \`${id}\` is already used by \`${owner}\` on the same page`,
					remedy: 'reword one of the two headings so the two anchors differ'
				});
				continue;
			}
			seen.set(id, unit.source);
			ids.add(id);
		}
	}

	return ids;
}

/**
 * The root-relative paths flagship copy may link to: every route the build
 * knows about, so a link to a page that stopped existing fails rather than
 * shipping a 404.
 */
function siteHrefs(docSite: DocSite): Set<string> {
	const hrefs = new Set<string>(EXTRA_ROUTES);
	for (const link of NAV_LINKS) hrefs.add(link.href);
	for (const page of docSite.pages) hrefs.add(page.href);
	for (const tool of TOOLS) hrefs.add(`/tools/${tool.slug}`);
	return hrefs;
}
