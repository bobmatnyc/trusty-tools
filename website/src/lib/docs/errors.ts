/**
 * Why: the doc reader is a build gate as well as a renderer (#5098). A manifest
 * row pointing at a deleted file, a relative link that no longer resolves, or a
 * duplicated route must stop the build rather than ship a 404 — and the person
 * who has to fix it needs the file, the line, what is wrong, and what to do,
 * not a stack trace. `scripts/check_public_docs.sh` already prints exactly that
 * shape (`FAIL <CODE> line N: …`); this module is its TypeScript counterpart so
 * the two gates read the same way in a build log.
 *
 * What: a `DocFailure` record, its one-line rendering, and the aggregate error
 * thrown once per build with every failure listed. Failures ACCUMULATE — a
 * build that breaks nine links reports nine, not the first one — because the
 * alternative is nine build/fix round-trips.
 *
 * Test: `errors.test.ts` pins the rendered line format; every other suite in
 * this directory asserts against the codes defined here.
 */

/** One build-stopping finding, addressed to whoever must repair it. */
export interface DocFailure {
	/** Screaming-kebab code, matching the vocabulary in `check_public_docs.sh`. */
	code: string;
	/** Repo-relative path of the file at fault. */
	file: string;
	/** 1-based line within `file`, when the finding has one. */
	line?: number;
	/** What is wrong, stated as fact. */
	problem: string;
	/** What to do about it. */
	remedy: string;
}

/** Renders one failure as `FAIL CODE file:line: problem — remedy`. */
export function formatFailure(failure: DocFailure): string {
	const location = failure.line === undefined ? failure.file : `${failure.file}:${failure.line}`;
	return `FAIL ${failure.code} ${location}: ${failure.problem} — ${failure.remedy}`;
}

/**
 * Why: SvelteKit surfaces a thrown error's `message` in the build log and
 * nothing else, so the whole report has to live in that one string.
 * What: an Error carrying every accumulated failure, its message being the
 * formatted report.
 */
export class DocBuildError extends Error {
	readonly failures: readonly DocFailure[];

	constructor(failures: readonly DocFailure[]) {
		const lines = failures.map(formatFailure);
		super(
			`documentation build gate: ${failures.length} finding(s)\n` +
				lines.join('\n') +
				'\nSee website/src/lib/docs/README.md for what each code means.'
		);
		this.name = 'DocBuildError';
		this.failures = failures;
	}
}

/** Throws a `DocBuildError` when anything was collected; a no-op otherwise. */
export function throwIfFailed(failures: readonly DocFailure[]): void {
	if (failures.length > 0) throw new DocBuildError(failures);
}
