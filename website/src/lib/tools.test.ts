import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { TOOLS } from './tools';
import { STABLE_SET } from './site';

/**
 * Why: the flagship pages assert things about crates. The two claims most
 * likely to rot silently are the ones the reader will act on — the `-p` flag
 * they paste into `cargo test`, and the `tctl install` target — because both
 * look plausible while being wrong. `crates/trusty-git-analytics` is package
 * `tga`, so the directory name is NOT the package name and cannot be assumed.
 *
 * What: re-derives each record's crate directory, package name, install
 * target, and docs route from the repository rather than from prose.
 *
 * Not covered here: the page copy itself. Prose claims were verified by hand
 * against each crate's clap enums and MCP descriptor tables; a unit test
 * cannot re-derive an English sentence.
 *
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../..');

describe('flagship tool records are grounded in the repository', () => {
	it('names a crate directory that exists', () => {
		expect(TOOLS.length).toBe(6);
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

	it('every install command targets a stable-set member', () => {
		for (const tool of TOOLS) {
			const target = tool.install.replace('tctl install', '').trim();
			expect(STABLE_SET, `${tool.name} installs ${target}`).toContain(target);
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

	it('has a route directory for every slug, and no duplicate slugs', () => {
		expect(new Set(TOOLS.map((t) => t.slug)).size).toBe(TOOLS.length);
		for (const tool of TOOLS) {
			expect(
				existsSync(path.join(HERE, '../routes/tools', tool.slug, '+page.svelte')),
				tool.slug
			).toBe(true);
		}
	});

	it('never names a retired binary or a `cp` install', () => {
		const prose = JSON.stringify(TOOLS);
		for (const banned of [
			'open-mpm',
			'trusty-mpmd',
			'trusty-mpm-tui',
			'trusty-mpm-telegram',
			'trusty-memory-core',
			'TRUSTY_ALLOW_UNLISTED',
			'search_code'
		]) {
			expect(prose, banned).not.toContain(banned);
		}
	});
});
