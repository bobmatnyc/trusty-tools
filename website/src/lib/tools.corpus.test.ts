import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { TOOLS } from './tools';

/**
 * Why: the flagship records claim things about crates, and the claims most
 * likely to rot silently are re-derived from the repository rather than trusted.
 * Every case here reads `crates/**`, `docs/**` or `src/content/**`, which the
 * `unit` project does not run for — those paths are the CORPUS job's triggers
 * (#8272).
 * What: re-derives each record's crate directory, package name, release state,
 * docs route, page source, and MCP tool count from the repository. The
 * repository-free cases stay in `tools.test.ts`.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../..');

describe('flagship tool records are grounded in the repository', () => {
	it('names a crate directory that exists', () => {
		expect(TOOLS.length).toBe(7);
		for (const tool of TOOLS) {
			expect(existsSync(path.join(REPO_ROOT, 'crates', tool.name, 'Cargo.toml')), tool.name).toBe(
				true
			);
		}
	});

	it("cargoPackage matches that crate's Cargo.toml name field", () => {
		for (const tool of TOOLS) {
			const manifest = readFileSync(
				path.join(REPO_ROOT, 'crates', tool.name, 'Cargo.toml'),
				'utf8'
			);
			// Anchored: a `[dependencies]` entry further down also matches `name`.
			const declared = manifest.match(/^name\s*=\s*"([^"]+)"/m)![1];
			expect(tool.cargoPackage, tool.name).toBe(declared);
		}
	});

	/**
	 * `$lib/changelog/site` fails the build for a crate whose CHANGELOG.md
	 * parses to zero releases, so `released` and the file have to agree: a
	 * record claiming a release the changelog does not carry breaks the build
	 * on `/whats-new`, and the reverse silently hides a shipped crate.
	 */
	it('marks a tool released only when its CHANGELOG.md carries a release', () => {
		for (const tool of TOOLS) {
			const changelog = readFileSync(
				path.join(REPO_ROOT, 'crates', tool.name, 'CHANGELOG.md'),
				'utf8'
			);
			const hasRelease = /^## \[(?!Unreleased\])/m.test(changelog);
			expect(tool.released, `${tool.name} CHANGELOG.md`).toBe(hasRelease);
		}
	});

	it('links only to doc pages the manifest actually publishes', () => {
		const routes = readFileSync(path.join(REPO_ROOT, 'docs/public-manifest.tsv'), 'utf8')
			.split('\n')
			.filter((line) => line.startsWith('PAGE\t'))
			.map((line) => `/docs${line.split('\t')[3]}`);
		for (const tool of TOOLS) {
			if (tool.docsPath === null) continue;
			expect(routes, tool.name).toContain(tool.docsPath);
		}
	});

	/**
	 * Since #6960 a flagship page is served one of two ways, and a record that
	 * matches NEITHER is a `/tools/<slug>` link on the landing page that leads
	 * to a 404: markdown under `src/content/tools/`, served by the `[slug]`
	 * route, or a hand-authored `+page.svelte` of its own (trusty-audit).
	 */
	it('has a page source for every slug, and no duplicate slugs', () => {
		expect(new Set(TOOLS.map((t) => t.slug)).size).toBe(TOOLS.length);
		for (const tool of TOOLS) {
			const markdown = existsSync(path.join(HERE, '../content/tools', `${tool.slug}.md`));
			const svelte = existsSync(path.join(HERE, '../routes/tools', tool.slug, '+page.svelte'));
			expect(markdown || svelte, tool.slug).toBe(true);
		}
	});

	/**
	 * The "MCP tools: N" fact card was wrong on `/tools/trusty-memory` — it said
	 * 45 against a dispatcher carrying 47, because a tool added to the crate
	 * changes nothing on the page. The count is the one fact-card number that is
	 * mechanically re-derivable, so it is derived here rather than trusted.
	 *
	 * Each crate exposes its tool set in a different shape, so the pattern is
	 * per-crate: trusty-memory dispatches by name in a match, trusty-search
	 * declares a descriptor table. Both are counted from the crate source the
	 * daemon actually serves.
	 */
	const TOOL_COUNT_SOURCES = [
		{
			slug: 'trusty-memory',
			source: 'crates/trusty-memory/src/tools/mod.rs',
			// Each dispatch arm: `"memory_remember" => handle_memory_remember(...)`.
			// The `other =>` catch-all carries no quotes and is not counted.
			pattern: /^\s*"[a-z_]+" =>/gm
		},
		{
			slug: 'trusty-search',
			source: 'crates/trusty-search/src/mcp/tools/descriptors.rs',
			// Each descriptor in the tools/list JSON: `"name": "search_lexical"`.
			pattern: /"name": "[a-z_]+"/g
		}
	];

	it.each(TOOL_COUNT_SOURCES)(
		'$slug fact card names the tool count its crate actually serves',
		({ slug, source, pattern }) => {
			const declared = readFileSync(path.join(REPO_ROOT, source), 'utf8').match(pattern);
			expect(declared, source).not.toBeNull();

			const tool = TOOLS.find((candidate) => candidate.slug === slug);
			const card = tool?.facts.find((fact) => fact.label === 'MCP tools');
			expect(card, `${slug} has an 'MCP tools' fact card`).toBeDefined();

			expect(Number(card!.value), `${slug} fact card vs ${source}`).toBe(declared!.length);
		}
	);
});
