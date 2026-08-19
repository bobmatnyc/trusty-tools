/**
 * Why: the single build-time entry point behind both surfaces — the "What's
 * new" strip on the landing page and the `/whats-new` page. Reading each
 * flagship's file once and memoising is not an optimisation: the landing page
 * and `/whats-new` would otherwise each re-read and re-parse thousands of
 * lines of markdown to reach the same answer.
 *
 * Scope is the RELEASED FLAGSHIPS and only them (`$lib/site`'s
 * `RELEASED_FLAGSHIPS`). The other crates' changelogs are read in the
 * repository by the people who need them; putting 21 more sections on a
 * marketing page would bury the ones a visitor came for. A flagship that
 * has never shipped is excluded by the same rule the NO_RELEASES gate below
 * enforces — it would have nothing to render.
 *
 * THE GATES. Each of these fails the build rather than rendering an empty
 * section, because an empty "What's New" is indistinguishable from "nothing
 * shipped" and the reader has no way to tell:
 *
 *   CHANGELOG-MISSING       the file is absent or unreadable
 *   CHANGELOG-NO-RELEASES   it parsed to zero release sections
 *   CHANGELOG-BAD-RELEASE   a `## [` heading never closes its bracket
 *   CHANGELOG-EMPTY-LATEST  the newest release carries no items
 *
 * There is no placeholder, no empty-array default, and no "no changes yet"
 * string anywhere below. A green build is therefore itself the evidence that
 * every section is populated.
 *
 * ONE finding is non-fatal and is logged rather than thrown:
 * `CHANGELOG-ROOT-RELATIVE-LINK`, a link the build resolved against the
 * repository root because the changelog's own directory could not resolve it
 * (#5640). It is accepted because refusing it would take the public site down
 * over a link shape a fragment author writes naturally, and it is logged
 * because the alternative is a green build quietly publishing a link to a
 * document nobody chose.
 *
 * This runs in Node at BUILD time only. Its output is prerendered into static
 * HTML, so the published page reads no file and opens no connection.
 *
 * Test: `site.test.ts` — the real released corpus builds clean, and each gate
 * is provoked from a temp-repo fixture.
 */

import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

import { RELEASED_FLAGSHIPS } from '../site';
import { GITHUB_REPO, findRepoRoot } from '../docs/repo';
import {
	CHANGELOG_CODES,
	reportWarnings,
	throwIfFailed,
	type ChangelogFailure,
	type ChangelogWarning
} from './errors';
import { parseChangelog, type ChangelogRelease } from './parse';

export interface CrateChangelog {
	/** Crate DIRECTORY name under `crates/`, and the section's `id` on the page. */
	name: string;
	/** `/tools/<slug>` — the flagship page for this crate. */
	slug: string;
	tagline: string;
	/** Repo-relative path this was read from. */
	source: string;
	/** The living changelog on GitHub. Deliberately unpinned — see below. */
	sourceUrl: string;
	/** Newest first. Never empty; a crate with no releases fails the build. */
	releases: ChangelogRelease[];
	/** `releases[0]`, named because both surfaces reach for it. */
	latest: ChangelogRelease;
}

export interface ChangelogSite {
	crates: CrateChangelog[];
	/** Where the other crates' changelogs live, for the note on the page. */
	cratesDirUrl: string;
}

/**
 * Why: the doc reader pins every in-repo link to a COMMIT SHA (`blob/<sha>`)
 * so a cited line can never drift out from under the prose quoting it. This
 * link is the opposite case and is unpinned ON PURPOSE: it points at a LIVING
 * document, and a reader who clicks "the full changelog" wants the one that is
 * current now, not a snapshot of the build. Do not "fix" this into a SHA.
 * What: the `main` permalink to a crate's changelog.
 */
function changelogUrl(relative: string): string {
	return `${GITHUB_REPO}/blob/main/${relative}`;
}

/** Same reasoning as `changelogUrl`: a living directory listing. */
const CRATES_DIR_URL = `${GITHUB_REPO}/tree/main/crates`;

let cached: ChangelogSite | undefined;

/** Rebuilds on the next call. Exposed for tests that build against fixtures. */
export function clearChangelogCache(): void {
	cached = undefined;
}

/**
 * Why/What/Test: see the module header.
 * @param repoRootOverride test seam; production always discovers the root.
 */
export function buildChangelogSite(repoRootOverride?: string): ChangelogSite {
	if (cached && repoRootOverride === undefined) return cached;

	const repoRoot = repoRootOverride ?? findRepoRoot();
	const failures: ChangelogFailure[] = [];
	const warnings: ChangelogWarning[] = [];
	const crates: CrateChangelog[] = [];

	for (const flagship of RELEASED_FLAGSHIPS) {
		const source = `crates/${flagship.name}/CHANGELOG.md`;
		const markdown = readChangelog(repoRoot, source, failures);
		if (markdown === undefined) continue;

		const parsed = parseChangelog(markdown, source, (relative) =>
			existsSync(path.join(repoRoot, relative))
		);
		failures.push(...parsed.failures);
		warnings.push(...parsed.warnings);

		const latest = parsed.releases[0];
		if (!latest) {
			failures.push({
				code: CHANGELOG_CODES.NO_RELEASES,
				file: source,
				problem: 'parsed to zero release sections, so this crate would render an empty section',
				remedy:
					'give the file at least one `## [<version>] — <YYYY-MM-DD>` heading, or set `released: false` on the crate in website/src/lib/tools.ts'
			});
			continue;
		}

		if (latest.itemCount === 0) {
			failures.push({
				code: CHANGELOG_CODES.EMPTY_LATEST,
				file: source,
				line: latest.line,
				problem: `the newest release \`${latest.version}\` has no items under any category, so the card strip would be blank`,
				remedy:
					'add the bullets this release shipped under a `### <Category>` heading — the strip shows the newest release and nothing else'
			});
			continue;
		}

		crates.push({
			name: flagship.name,
			slug: flagship.slug,
			tagline: flagship.tagline,
			source,
			sourceUrl: changelogUrl(source),
			releases: parsed.releases,
			latest
		});
	}

	// Warnings first: a build that is about to throw should still say what it
	// accepted on the way, and the log is the only channel either report has.
	reportWarnings(warnings);
	throwIfFailed(failures);

	const site: ChangelogSite = { crates, cratesDirUrl: CRATES_DIR_URL };
	if (repoRootOverride === undefined) cached = site;
	return site;
}

function readChangelog(
	repoRoot: string,
	source: string,
	failures: ChangelogFailure[]
): string | undefined {
	try {
		return readFileSync(path.join(repoRoot, source), 'utf8');
	} catch {
		failures.push({
			code: CHANGELOG_CODES.MISSING,
			file: source,
			problem: 'a flagship crate has no readable CHANGELOG.md',
			remedy:
				'restore the file, or set `released: false` on the crate in website/src/lib/tools.ts — the What’s New page renders no placeholder for a crate it cannot read'
		});
		return undefined;
	}
}

/**
 * How many releases per crate carry their full item detail on `/whats-new`.
 *
 * Why a cap at all: the flagship changelogs are hundreds of kB of markdown
 * combined, and a page carrying every item of all 234 releases weighed
 * 2.4 MB — because SvelteKit
 * inlines the load's data for hydration ON TOP of the rendered HTML, and emits
 * it a third time as `__data.json`. That is a worse way to read the archive
 * than the file this page already links to. Detail for what shipped recently,
 * an index of everything else, and one click to the whole file.
 */
export const DETAILED_RELEASES = 3;

/** One release reduced to its heading — what an older release shows. */
export interface ReleaseSummary {
	version: string;
	date?: string;
	title?: string;
}

export interface CrateSection {
	name: string;
	slug: string;
	tagline: string;
	source: string;
	sourceUrl: string;
	/** Every release the file has, including the ones only summarised. */
	releaseCount: number;
	/** Newest first, with categories, items, and prose. */
	detailed: ChangelogRelease[];
	/** The rest, heading only, newest first. Never overlaps `detailed`. */
	earlier: ReleaseSummary[];
}

/**
 * Why: `/whats-new` must not ship what it does not render — see
 * `DETAILED_RELEASES`. Projecting here rather than in the component is what
 * keeps the trimmed payload out of the hydration script as well as out of the
 * HTML.
 * What: `buildChangelogSite`, with each crate split into detailed releases and
 * summarised earlier ones. Every release appears in exactly one of the two.
 * Test: `site.test.ts` (`the /whats-new projection`).
 */
export function whatsNewSections(repoRootOverride?: string): {
	crates: CrateSection[];
	cratesDirUrl: string;
} {
	const site = buildChangelogSite(repoRootOverride);
	return {
		cratesDirUrl: site.cratesDirUrl,
		crates: site.crates.map((crate) => ({
			name: crate.name,
			slug: crate.slug,
			tagline: crate.tagline,
			source: crate.source,
			sourceUrl: crate.sourceUrl,
			releaseCount: crate.releases.length,
			detailed: crate.releases.slice(0, DETAILED_RELEASES),
			earlier: crate.releases.slice(DETAILED_RELEASES).map(({ version, date, title }) => ({
				version,
				date,
				title
			}))
		}))
	};
}

/**
 * Why: the landing-page strip wants three lines, not a release. Deriving them
 * here keeps the truncation rule (`take what exists, never pad`) in one place
 * rather than in a component.
 * What: up to `limit` items from the newest release, each tagged with its
 * category BUCKET — the short key, not the full label, because
 * `Fixed (closes #1373)` does not fit a card.
 * Test: `site.test.ts` (`the landing-page strip`).
 */
export function stripItems(
	release: ChangelogRelease,
	limit = 3
): { category: string; text: string }[] {
	const lines: { category: string; text: string }[] = [];
	for (const category of release.categories) {
		for (const item of category.items) {
			if (lines.length === limit) return lines;
			lines.push({ category: category.key, text: item.text });
		}
	}
	return lines;
}
