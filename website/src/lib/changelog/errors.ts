/**
 * Why: "What's New" is a DERIVED view of files this site does not own. If
 * one of them stops parsing, the honest outcome is a red build — a page that
 * renders an empty strip instead is instrumentation reporting success over a
 * failure, and the reader has no way to tell the difference between "nothing
 * shipped" and "the reader broke". So every condition that would produce an
 * empty section is a build gate, and all of them are collected in one pass so
 * a single build log names every problem.
 *
 * What: the changelog gate's failure vocabulary. The record shape, the
 * one-line rendering, and `throwIfFailed` are the doc reader's
 * (`../docs/errors.ts`) — reused rather than copied, so both gates read
 * identically in a build log and neither can drift. Only the aggregate error
 * differs, because its message names this module and its own README.
 *
 * Test: `parse.test.ts` and `site.test.ts` assert against the codes below.
 */

import { formatFailure, type DocFailure } from '../docs/errors';

/** One build-stopping finding in a crate's `CHANGELOG.md`. */
export type ChangelogFailure = DocFailure;

/** Every code this gate can emit, with what provokes it. */
export const CHANGELOG_CODES = {
	/** The crate's `CHANGELOG.md` is absent or unreadable. */
	MISSING: 'CHANGELOG-MISSING',
	/** The file read, but no `## [version]` section was found in it. */
	NO_RELEASES: 'CHANGELOG-NO-RELEASES',
	/** A heading opens `## [` and does not close the bracket. */
	BAD_RELEASE: 'CHANGELOG-BAD-RELEASE',
	/** The newest release carries no items — the card strip would be blank. */
	EMPTY_LATEST: 'CHANGELOG-EMPTY-LATEST',
	/** A link in the prose points somewhere this site cannot send a reader. */
	BAD_LINK: 'CHANGELOG-BAD-LINK'
} as const;

/**
 * Why: SvelteKit surfaces a thrown error's `message` in the build log and
 * nothing else, so the whole report has to live in that one string.
 * What: an Error carrying every accumulated failure, its message being the
 * formatted report.
 */
export class ChangelogBuildError extends Error {
	readonly failures: readonly ChangelogFailure[];

	constructor(failures: readonly ChangelogFailure[]) {
		super(
			`changelog build gate: ${failures.length} finding(s)\n` +
				failures.map(formatFailure).join('\n') +
				'\nSee website/src/lib/changelog/README.md for what each code means.'
		);
		this.name = 'ChangelogBuildError';
		this.failures = failures;
	}
}

/** Throws a `ChangelogBuildError` when anything was collected; a no-op otherwise. */
export function throwIfFailed(failures: readonly ChangelogFailure[]): void {
	if (failures.length > 0) throw new ChangelogBuildError(failures);
}
