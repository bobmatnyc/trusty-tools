/**
 * Why: the flagship `CHANGELOG.md` files ARE the source of truth for what
 * shipped. This site only makes them readable — nothing here writes back to a
 * changelog, and nothing here invents a release. That means the parser has to
 * take the corpus as it actually is rather than as Keep a Changelog describes
 * it, because 502 headings of hand-written history disagree with the template
 * in four ways that a strict parser would silently discard:
 *
 *   1. 49 release headings carry a TITLE where the date should be
 *      (`## [0.1.46] — 4 indexing speed optimizations`), and three carry
 *      neither a semver version nor a date (`## [consolidation] — 2026-05-26`,
 *      `## [0.4.0] and prior`, `## [2026-05-11]`).
 *   2. Roughly 40 category headings carry a qualifier —
 *      `### Fixed (closes #1373)`, `### Deprecated: verbose … (issue #1205)`.
 *      Bucketing is by the LEADING WORD; the full heading stays as the label.
 *   3. Categories outside the assembler's canonical set (`Notes`, `Internal`,
 *      `Highlights`, …) are hand-written history, not errors. They render
 *      under their literal heading.
 *   4. A release body is not only bullets: 114 paragraphs, 42 table rows, 40
 *      fenced blocks and 15 block quotes sit under category headings, and four
 *      crates open their newest release with several paragraphs of prose
 *      BEFORE the first category. All of it is preserved, in source order.
 *
 * THE LINK-REFERENCE TRAP. `crates/trusty-search/CHANGELOG.md` ends with 58
 * link-reference definitions pointing at the PRE-MONOREPO repository
 * (`[0.3.36]: https://github.com/bobmatnyc/trusty-search/compare/…`), and
 * `crates/trusty-analyze/CHANGELOG.md` has 13 more. Their labels match the
 * `## [0.3.36]` heading text exactly, so remark resolves each version heading
 * into a link — to a dead repository in the first case, and to somewhere a
 * heading has no business linking in the second. `stripLinkDefinitions` blanks
 * them before parsing, which leaves the heading as the plain text
 * `[0.3.36] — 2026-05-14`.
 *
 * What: `parseChangelog`, a pure function from one file's markdown to its
 * releases plus any build-stopping findings. It performs no I/O, so every
 * failure path is reachable from a string fixture. Parsing runs the doc
 * reader's markdown pipeline (`../docs/render`) — there is no second
 * remark/rehype stack in this module.
 *
 * Test: `parse.test.ts`.
 */

import path from 'node:path';

import { toString as hastToString } from 'hast-util-to-string';
import { visit } from 'unist-util-visit';
import type { Element, Root, RootContent } from 'hast';

import { classifyAbsoluteHref } from '../docs/links';
import { ALLOWED_ELEMENTS, parsePage, stringifyHast } from '../docs/render';
import { GITHUB_REPO } from '../docs/repo';
import { CHANGELOG_CODES, type ChangelogFailure } from './errors';

/** One bullet under a category heading. */
export interface ChangelogItem {
	/** Inline markdown as HTML — links, code spans, emphasis, nested lists. */
	html: string;
	/** The same content as plain text, for surfaces that cannot host markup. */
	text: string;
}

/**
 * A category's body, in source order. A lead-in paragraph before its list and
 * a table after it both keep their position; nothing is reordered or dropped.
 */
export type ChangelogBlock =
	{ kind: 'items'; items: ChangelogItem[] } | { kind: 'html'; html: string };

export interface ChangelogCategory {
	/** Bucket: the leading word, normalised to canonical casing when it matches. */
	key: string;
	/** The heading exactly as written, qualifier and all. */
	label: string;
	blocks: ChangelogBlock[];
	/** Every bullet under this heading, flattened — what the card strip reads. */
	items: ChangelogItem[];
}

export interface ChangelogRelease {
	/** Whatever sat inside the brackets: a semver, or `consolidation`, or a date. */
	version: string;
	/** `YYYY-MM-DD`, when the heading carries one. 69 of 234 releases do not. */
	date?: string;
	/** The rest of the heading once the version and date are removed. */
	title?: string;
	/** Prose between the release heading and its first category, as HTML. */
	preambleHtml?: string;
	categories: ChangelogCategory[];
	/** Bullets across every category. The build gate reads this for the latest. */
	itemCount: number;
	/** 1-based line of the release heading, for failure reporting. */
	line: number;
}

export interface ParsedChangelog {
	releases: ChangelogRelease[];
	failures: ChangelogFailure[];
}

/**
 * The categories `scripts/assemble-changelog.sh:87` emits, in the order it
 * emits them. Anything else in a file is hand-written history and renders
 * under its own heading — an unknown category is not a build failure.
 */
const CANONICAL_CATEGORIES = [
	'Breaking',
	'Added',
	'Fixed',
	'Performance',
	'Changed',
	'Removed',
	'Security',
	'Documentation'
] as const;

const CANONICAL_BY_LOWER = new Map(
	CANONICAL_CATEGORIES.map((name) => [name.toLowerCase(), name as string])
);

/** A link-reference definition at the start of a line: `[label]: destination`. */
const LINK_DEFINITION = /^\[[^\]]+\]:\s*\S/;
/** Opening or closing fence of a code block, at any indent markdown accepts. */
const FENCE = /^ {0,3}(?:```|~~~)/;
/** ISO date, wherever it sits in a heading. */
const ISO_DATE = /\d{4}-\d{2}-\d{2}/;
/** `[version]` followed by whatever else the heading carries. */
const RELEASE_HEADING = /^\[([^\]]+)\](.*)$/s;
/** A separator between the version and the rest: em dash, en dash, or hyphen. */
const LEADING_SEPARATOR = /^[\s—–·-]+/;
const TRAILING_SEPARATOR = /[\s—–·-]+$/;

/**
 * Why: see THE LINK-REFERENCE TRAP in the module header.
 * What: replaces every link-reference definition with an EMPTY LINE rather
 * than deleting it, so every node's reported line number still matches the
 * file on disk. Definitions inside a fenced block are left alone — a changelog
 * that shows markdown in a code sample must keep showing it.
 * Test: `parse.test.ts` (`strips the link-reference footer …`).
 */
export function stripLinkDefinitions(markdown: string): string {
	let inFence = false;
	return markdown
		.split('\n')
		.map((line) => {
			if (FENCE.test(line)) {
				inFence = !inFence;
				return line;
			}
			if (inFence) return line;
			return LINK_DEFINITION.test(line) ? '' : line;
		})
		.join('\n');
}

/**
 * Why: `### Fixed (closes #1373)` and `### Fixed` are the same bucket to a
 * reader and different strings to a parser. The leading word is the bucket;
 * the whole heading stays as the label so the qualifier is never lost.
 * What: the leading word, stripped of a trailing colon and normalised to the
 * canonical spelling when it matches one case-insensitively (`BREAKING` →
 * `Breaking`). An unrecognised word is returned as written.
 * Test: `parse.test.ts` (`buckets a qualified category heading …`).
 */
export function categoryKey(label: string): string {
	const word =
		label
			.trim()
			.split(/\s+/)[0]
			?.replace(/[:.,;]+$/, '') ?? '';
	return CANONICAL_BY_LOWER.get(word.toLowerCase()) ?? word;
}

interface ReleaseHeading {
	version: string;
	date?: string;
	title?: string;
}

/**
 * Why: the release-heading grammar in this corpus is `[label]`, optionally a
 * separator, then a date and/or a free-text title in either order of absence.
 * Requiring a semver and a date would drop 49 of trusty-search's 118 releases.
 * What: parses the heading's TEXT (the `## ` marker already removed by the
 * markdown parser). Returns `undefined` when the bracket never closes, which
 * is what fails the build.
 * Test: `parse.test.ts` (`release headings`).
 */
export function parseReleaseHeading(text: string): ReleaseHeading | undefined {
	const trimmed = text.trim();
	if (trimmed === '') return undefined;

	let version: string;
	let remainder: string;
	if (trimmed.startsWith('[')) {
		const match = RELEASE_HEADING.exec(trimmed);
		if (!match) return undefined;
		version = match[1].trim();
		if (version === '') return undefined;
		remainder = match[2];
	} else {
		// No file currently writes an unbracketed release heading. Reading the
		// whole text as the label keeps a future `## Unreleased` rendering
		// instead of vanishing, and leaves `## [` as the only failing shape.
		version = trimmed;
		remainder = '';
	}

	const rest = remainder.replace(LEADING_SEPARATOR, '').trim();
	const dateMatch = ISO_DATE.exec(rest);
	// `## [2026-05-11]` (trusty-git-analytics) puts the date in the version
	// slot. Reading it as the date too lets the component suppress the repeat.
	const date = dateMatch?.[0] ?? ISO_DATE.exec(version)?.[0];

	const title = (dateMatch ? rest.replace(dateMatch[0], '') : rest)
		.replace(LEADING_SEPARATOR, '')
		.replace(TRAILING_SEPARATOR, '')
		.trim();

	return { version, date, title: title === '' ? undefined : title };
}

/**
 * Why/What/Test: see the module header; `parse.test.ts` covers each shape.
 * @param file repo-relative path of the changelog. Names the failure records,
 *   and is the base every relative link inside it resolves against.
 * @param probe repo-relative existence check, used only for those links. The
 *   default rejects everything, which is right for a fixture with none.
 */
export function parseChangelog(
	markdown: string,
	file: string,
	probe: (relative: string) => boolean = () => false
): ParsedChangelog {
	const { tree } = parsePage(stripLinkDefinitions(markdown));
	const failures: ChangelogFailure[] = [];
	const releases: ChangelogRelease[] = [];

	harden(tree, file, probe, failures);

	let release: ChangelogRelease | undefined;
	let category: ChangelogCategory | undefined;
	let pending: RootContent[] = [];

	/** Flushes buffered non-list blocks into wherever they were written. */
	const flush = () => {
		if (pending.length === 0) return;
		const html = stringifyHast(pending);
		pending = [];
		if (category) category.blocks.push({ kind: 'html', html });
		else if (release) release.preambleHtml = (release.preambleHtml ?? '') + html;
	};

	for (const node of tree.children) {
		if (node.type !== 'element') continue;
		const element = node as Element;

		if (element.tagName === 'h1') continue;

		// A `---` rule separates releases in every one of these files. It is
		// structure, not content, so it is the one node kind dropped outright.
		if (element.tagName === 'hr') continue;

		if (element.tagName === 'h2') {
			flush();
			category = undefined;
			release = startRelease(element, file, failures);
			if (release) releases.push(release);
			continue;
		}

		if (element.tagName === 'h3' && release) {
			flush();
			const label = hastToString(element).trim();
			category = { key: categoryKey(label), label, blocks: [], items: [] };
			release.categories.push(category);
			continue;
		}

		if ((element.tagName === 'ul' || element.tagName === 'ol') && category) {
			flush();
			const items = element.children
				.filter((child): child is Element => child.type === 'element' && child.tagName === 'li')
				.map((li) => ({ html: stringifyHast(li.children), text: hastToString(li).trim() }));
			if (items.length > 0) {
				category.blocks.push({ kind: 'items', items });
				category.items.push(...items);
			}
			continue;
		}

		if (release) pending.push(element);
	}
	flush();

	for (const entry of releases) {
		entry.itemCount = entry.categories.reduce((total, cat) => total + cat.items.length, 0);
	}

	return { releases, failures };
}

/**
 * Why: four problems in changelog prose would each ship a defect quietly.
 *
 *   1. A RELATIVE link (`[spec](../../docs/specs/foo.md)`) is written to be
 *      followed on GitHub. Rendered verbatim on `/whats-new` it resolves
 *      against the SITE root, so `../../docs/specs/foo.md` becomes
 *      `/docs/specs/foo.md` — a route this site does not have. SvelteKit's
 *      prerenderer already refuses to build with one, which is how the three
 *      in `crates/trusty-mpm/CHANGELOG.md` were found.
 *   2. `<path>` written outside backticks parses as an unknown element and
 *      renders as NOTHING — `run trusty-search index <path>` would publish as
 *      `run trusty-search index`. Eight of these are in the corpus right now.
 *   3. An image would be a third-party request from a site that makes none.
 *   4. An absolute link using a scheme this site refuses to publish
 *      (`javascript:`, `file:`, …) would render as a live href. Checked via
 *      `classifyAbsoluteHref` — the same allowlist the doc reader applies, so
 *      the two never drift (`../docs/links.ts`).
 *
 * What: rewrites every relative link into a `blob/main` link on the crate's
 * own path, and fails the build on anything it cannot resolve. Mutates the
 * tree in place, before any of it is serialised.
 *
 * Case 2 RECOVERS rather than failing, unlike the identical hazard in the doc
 * reader. The difference is who can fix it: a published page's author can add
 * the backticks, but a changelog is append-only history that this site must
 * not edit, so a build gate there would be unfixable. The element is turned
 * back into the literal text the author wrote and its contents are kept, which
 * loses nothing.
 *
 * Test: `parse.test.ts` (`links and metavariables inside an item`).
 */
function harden(
	tree: Root,
	file: string,
	probe: (relative: string) => boolean,
	failures: ChangelogFailure[]
): void {
	const dir = path.posix.dirname(file);

	visit(tree, 'element', (node: Element, index, parent) => {
		const line = node.position?.start.line;

		if (!ALLOWED_ELEMENTS.has(node.tagName)) {
			if (!parent || typeof index !== 'number') return;
			parent.children.splice(
				index,
				1,
				{ type: 'text', value: `<${node.tagName}>` },
				...node.children
			);
			// Continue from the replacement, so a metavariable nested inside
			// another one (`tm-<project>-<n>`) is recovered as well.
			return index;
		}

		if (node.tagName === 'img') {
			failures.push({
				code: CHANGELOG_CODES.BAD_LINK,
				file,
				line,
				problem:
					'an image in a changelog would be a third-party request from a page that makes none',
				remedy:
					'describe it in words, or link to it — the published site fetches nothing at runtime'
			});
			return;
		}

		if (node.tagName !== 'a') return;
		const href = node.properties?.href;
		if (typeof href !== 'string' || href === '') return;

		const absolute = classifyAbsoluteHref(href);
		if (absolute.kind === 'unsafe-scheme') {
			failures.push({
				code: CHANGELOG_CODES.BAD_LINK,
				file,
				line,
				problem: `\`${href}\` uses the \`${absolute.scheme}\` scheme, which this site will not publish as a link`,
				remedy: 'use an https:// (or http://) URL, or a path relative to the changelog'
			});
			return;
		}
		if (absolute.kind === 'allowed') return;

		const hash = href.indexOf('#');
		const rawTarget = hash === -1 ? href : href.slice(0, hash);
		const fragment = hash === -1 ? '' : href.slice(hash);

		if (rawTarget === '' || rawTarget.startsWith('/')) {
			failures.push({
				code: CHANGELOG_CODES.BAD_LINK,
				file,
				line,
				problem: `\`${href}\` means one thing on GitHub and another on this site`,
				remedy: 'write it as an absolute https:// URL, or as a path relative to the changelog'
			});
			return;
		}

		const target = path.posix.normalize(path.posix.join(dir, rawTarget));

		// A target outside the repository is a dead stop, never a guess. An
		// earlier revision stripped the leading `../` and accepted whatever it
		// landed on if that path existed — which turned
		// `[the README](../../../README.md)` in a trusty-search changelog into a
		// confident link to the repo-root README, with zero failures reported.
		// A green build has to mean every link resolved as written.
		if (target.startsWith('..')) {
			failures.push({
				code: CHANGELOG_CODES.BAD_LINK,
				file,
				line,
				problem: `\`${href}\` resolves to \`${target}\`, outside the repository`,
				remedy: `count the \`../\` from \`${dir}/\`, or write it as an absolute https:// URL`
			});
			return;
		}

		if (!probe(target)) {
			failures.push({
				code: CHANGELOG_CODES.BAD_LINK,
				file,
				line,
				problem: `\`${href}\` resolves to \`${target}\`, which is not in the repository`,
				remedy:
					'point it at the file’s current path, write an absolute https:// URL, or drop the link and keep its text'
			});
			return;
		}

		// `blob/main`, not a pinned SHA: see `changelogUrl` in `site.ts`.
		node.properties = {
			...node.properties,
			href: `${GITHUB_REPO}/blob/main/${target}${fragment}`,
			rel: ['noreferrer', 'noopener']
		};
	});
}

/** Turns one `h2` into a release, or records why it is not one. */
function startRelease(
	heading: Element,
	file: string,
	failures: ChangelogFailure[]
): ChangelogRelease | undefined {
	const text = hastToString(heading).trim();
	const line = heading.position?.start.line;
	const parsed = parseReleaseHeading(text);

	if (!parsed) {
		failures.push({
			code: CHANGELOG_CODES.BAD_RELEASE,
			file,
			line,
			problem: `\`## ${text}\` opens a release section and never closes its \`[version]\` bracket`,
			remedy:
				'write the heading as `## [<version>] — <YYYY-MM-DD>`; the date and a trailing title are both optional, the bracketed version is not'
		});
		return undefined;
	}

	return { ...parsed, categories: [], itemCount: 0, line: line ?? 0 };
}
