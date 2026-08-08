/**
 * Why: everything the doc reader reads lives ABOVE `website/` — the manifest
 * and the 27 markdown sources — and it must all be read at BUILD time. A
 * Vercel serverless function cannot reliably reach repo files outside its
 * bundle, so there is deliberately no runtime file-reading route; this module
 * exists only inside `+page.server.ts`/`+layout.server.ts` loads that are
 * prerendered, and its output is baked into static HTML.
 *
 * What: locates the repository root, probes and reads paths under it, and
 * resolves the commit SHA that in-repo links are pinned to. The SHA comes from
 * the build environment or from `git`; it is never fetched over the network,
 * because the published site must make no runtime connections at all.
 *
 * Test: `repo.test.ts` — root discovery from a nested cwd, the env precedence
 * chain for the SHA, and the failure when no SHA can be established.
 */

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';

import { DocBuildError } from './errors';

/** The one repository this site publishes from. */
export const GITHUB_REPO = 'https://github.com/bobmatnyc/trusty-tools';

/** The file whose presence identifies the repository root. */
const ROOT_MARKER = 'docs/public-manifest.tsv';

/**
 * Why: `pnpm build` runs with cwd `website/`, `vitest` runs with cwd
 * `website/`, and a stray invocation from the repo root should still work.
 * Deriving the root from `import.meta.url` does not survive the SSR bundle
 * (prerendering executes from `.svelte-kit/output/server/chunks/`), so the
 * search walks up from cwd instead.
 * What: nearest ancestor of `from` containing `docs/public-manifest.tsv`.
 * Test: `repo.test.ts` (`finds the repository root from a nested directory`).
 */
export function findRepoRoot(from: string = process.env.TRUSTY_REPO_ROOT ?? process.cwd()): string {
	let dir = path.resolve(from);
	for (let depth = 0; depth < 8; depth += 1) {
		if (existsSync(path.join(dir, ROOT_MARKER))) return dir;
		const parent = path.dirname(dir);
		if (parent === dir) break;
		dir = parent;
	}
	throw new DocBuildError([
		{
			code: 'NO-REPO-ROOT',
			file: ROOT_MARKER,
			problem: `no ancestor of \`${path.resolve(from)}\` contains \`${ROOT_MARKER}\``,
			remedy:
				'run the build from `website/` inside a full checkout; on Vercel, turn ON "Include source files outside of the Root Directory"'
		}
	]);
}

/** What a repo-relative path currently is. */
export type RepoEntryKind = 'file' | 'dir' | 'missing';

/** Classifies a repo-relative path without following it out of the repo. */
export function probeRepoEntry(repoRoot: string, relative: string): RepoEntryKind {
	const absolute = path.join(repoRoot, relative);
	if (!existsSync(absolute)) return 'missing';
	return statSync(absolute).isDirectory() ? 'dir' : 'file';
}

/** Reads a repo-relative UTF-8 file. */
export function readRepoFile(repoRoot: string, relative: string): string {
	return readFileSync(path.join(repoRoot, relative), 'utf8');
}

const SHA = /^[0-9a-f]{40}$/;

/**
 * Why: links into unpublished parts of the repo are pinned to a COMMIT, never
 * to `blob/main` — a `main` link silently retargets as the file changes, so the
 * prose on a published page and the lines it points at drift apart with nobody
 * noticing. Pinning costs a build-time lookup and buys a permanent guarantee
 * that the target is what the author was describing.
 * What: the build's commit SHA, from the platform's environment first (Vercel,
 * then GitHub Actions) and otherwise from `git rev-parse HEAD`. No network.
 * Test: `repo.test.ts` (`prefers VERCEL_GIT_COMMIT_SHA`, `falls back to git`,
 * `fails the build when no SHA is available`).
 */
export function resolveCommitSha(repoRoot: string, env: NodeJS.ProcessEnv = process.env): string {
	for (const key of ['TRUSTY_DOCS_COMMIT_SHA', 'VERCEL_GIT_COMMIT_SHA', 'GITHUB_SHA']) {
		const value = env[key]?.trim().toLowerCase();
		if (value && SHA.test(value)) return value;
	}

	try {
		const value = execFileSync('git', ['rev-parse', 'HEAD'], {
			cwd: repoRoot,
			encoding: 'utf8',
			stdio: ['ignore', 'pipe', 'ignore']
		})
			.trim()
			.toLowerCase();
		if (SHA.test(value)) return value;
	} catch {
		/* Falls through to the failure below, which names the remedy. */
	}

	throw new DocBuildError([
		{
			code: 'NO-COMMIT-SHA',
			file: 'website/src/lib/docs/repo.ts',
			problem:
				'no build commit SHA is available from VERCEL_GIT_COMMIT_SHA, GITHUB_SHA, or `git rev-parse HEAD`',
			remedy:
				'build from a git checkout, or set TRUSTY_DOCS_COMMIT_SHA to the 40-character commit being published — links into the repository are pinned to it and must never fall back to `blob/main`'
		}
	]);
}

/** Permalink to a file in the repository, pinned to `sha`. */
export function blobUrl(sha: string, relative: string, fragment = ''): string {
	return `${GITHUB_REPO}/blob/${sha}/${relative}${fragment}`;
}

/** Permalink to a directory in the repository, pinned to `sha`. */
export function treeUrl(sha: string, relative: string): string {
	return `${GITHUB_REPO}/tree/${sha}/${relative}`;
}
