/**
 * Why: the 27 published pages cross-link by relative `.md` path into a tree of
 * 450-odd files, most of which are NOT published — several pages end in doc-map
 * tables pointing at `spec/`, `research/`, `sessions/`, `decisions/`. Rendered
 * naively those become dead links whose text names an internal document, which
 * is worse than no link: the reader learns a document exists and is given no
 * way to reach it. Every relative target therefore gets CLASSIFIED, and every
 * class has a defined destination.
 *
 * THE RULE, in one sentence: a relative link resolves to a site route when its
 * target is on the manifest, to a commit-pinned GitHub permalink when its
 * target exists anywhere else in the repository, and fails the build otherwise.
 *
 *   1. on the manifest              → `/docs<route>`, fragment preserved
 *   2. elsewhere in the repository  → `blob/<sha>/…` (file) or `tree/<sha>/…`
 *      (directory). Covers BOTH unpublished `docs/` pages and targets outside
 *      `docs/` entirely (`../../README.md`, `../../crates/…`): neither can
 *      become a site route, and the treatment is identical because the reader's
 *      situation is identical — a real document, not published here.
 *   3. `#fragment` only             → left alone, but checked against the
 *      page's own headings
 *   4. `other.md#fragment`          → case 1 or 2, and when case 1 the fragment
 *      is checked against the TARGET page's headings
 *   5. missing, or outside the repo → BROKEN-LINK / ESCAPES-REPO, build fails
 *
 * Permalink rather than dropping the link: dropping keeps the sentence ("see
 * `docs/specs/foo.md`") while removing every way to act on it, so the reader is
 * strictly worse off than with a link. Pinning to the SHA — never `blob/main`,
 * which silently retargets as lines shift — means the target is exactly the
 * revision the published prose was written against.
 *
 * What: `resolveLink`, a pure function from one raw href plus context to either
 * a resolved destination or a `DocFailure`. It performs no I/O; existence is a
 * caller-supplied probe, which is what makes every class testable from fixtures.
 *
 * Test: `links.test.ts` — one case per class above, plus the anchor cases and
 * the two build-failing ones.
 */

import path from 'node:path';

import type { DocFailure } from './errors';
import type { DocPage } from './manifest';
import { routeToHref } from './manifest';
import { blobUrl, treeUrl, type RepoEntryKind } from './repo';

/** Which of the five classes a link fell into. */
export type LinkClass = 'external' | 'anchor' | 'site' | 'repo-file' | 'repo-dir';

export interface ResolvedLink {
	class: LinkClass;
	href: string;
	/** True when following it leaves this site. Drives `rel` and the visual cue. */
	external: boolean;
}

export interface LinkContext {
	/** Repo-relative path of the page the link was written in. */
	fromSource: string;
	/** Manifest sources, for class 1. */
	bySource: ReadonlyMap<string, DocPage>;
	/** Filesystem probe for repo-relative paths, for class 2. */
	probe: (relative: string) => RepoEntryKind;
	/** Build commit the permalinks are pinned to. */
	commitSha: string;
	/** Heading ids of a published source, for anchor checking. */
	headingIds: (source: string) => ReadonlySet<string>;
	/** Line in `fromSource` the link sits on, for failure reporting. */
	line?: number;
}

export type LinkOutcome = { link: ResolvedLink } | { failure: DocFailure };

/** Anything with a scheme, or a protocol-relative URL, leaves the repository. */
const ABSOLUTE = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

function fail(
	ctx: LinkContext,
	code: string,
	problem: string,
	remedy: string
): { failure: DocFailure } {
	return { failure: { code, file: ctx.fromSource, line: ctx.line, problem, remedy } };
}

/**
 * Why: see the module header — this is where the rule is applied.
 * What: classifies one href and returns its destination, or the failure that
 * stops the build.
 * Test: `links.test.ts`.
 */
export function resolveLink(raw: string, ctx: LinkContext): LinkOutcome {
	const href = raw.trim();

	if (href === '') {
		return fail(
			ctx,
			'EMPTY-LINK',
			'a link has an empty destination',
			'give it a target, or remove the link and keep the text'
		);
	}

	if (ABSOLUTE.test(href)) {
		return { link: { class: 'external', href, external: true } };
	}

	if (href.startsWith('#')) {
		const fragment = decodeAnchor(href.slice(1));
		if (!ctx.headingIds(ctx.fromSource).has(fragment)) {
			return fail(
				ctx,
				'BROKEN-ANCHOR',
				`\`${href}\` names no heading on this page`,
				'fix the anchor to match a heading on this page, or link to the page that has it'
			);
		}
		return { link: { class: 'anchor', href, external: false } };
	}

	if (href.startsWith('/')) {
		return fail(
			ctx,
			'ABSOLUTE-PATH-LINK',
			`\`${href}\` is a root-relative path, which means one thing on GitHub and another on the site`,
			'write the target relative to this file instead'
		);
	}

	const hash = href.indexOf('#');
	const rawTarget = hash === -1 ? href : href.slice(0, hash);
	const fragment = hash === -1 ? '' : href.slice(hash);
	const target = path.posix.normalize(
		path.posix.join(path.posix.dirname(ctx.fromSource), decodeTarget(rawTarget))
	);

	if (target.startsWith('..')) {
		return fail(
			ctx,
			'ESCAPES-REPO',
			`\`${href}\` resolves to \`${target}\`, outside the repository`,
			'link to a file inside the repository, or use an absolute https:// URL'
		);
	}

	const published = ctx.bySource.get(target);
	if (published) {
		if (fragment !== '') {
			const anchor = decodeAnchor(fragment.slice(1));
			if (!ctx.headingIds(target).has(anchor)) {
				return fail(
					ctx,
					'BROKEN-ANCHOR',
					`\`${href}\` names no heading in \`${target}\``,
					`fix the anchor to match a heading in \`${target}\`, or drop the \`${fragment}\` suffix`
				);
			}
		}
		return {
			link: { class: 'site', href: routeToHref(published.route) + fragment, external: false }
		};
	}

	switch (ctx.probe(target)) {
		case 'file':
			return {
				link: {
					class: 'repo-file',
					href: blobUrl(ctx.commitSha, target, fragment),
					external: true
				}
			};
		case 'dir':
			return { link: { class: 'repo-dir', href: treeUrl(ctx.commitSha, target), external: true } };
		default:
			return fail(
				ctx,
				'BROKEN-LINK',
				`\`${href}\` resolves to \`${target}\`, which does not exist`,
				"point it at the file's current path, or delete the link and keep its text"
			);
	}
}

/** Percent-decoding, tolerant of the malformed sequences prose sometimes carries. */
function decodeTarget(value: string): string {
	try {
		return decodeURIComponent(value);
	} catch {
		return value;
	}
}

/** GitHub lower-cases anchors; matching that keeps a doc's own links working. */
function decodeAnchor(value: string): string {
	return decodeTarget(value).toLowerCase();
}
