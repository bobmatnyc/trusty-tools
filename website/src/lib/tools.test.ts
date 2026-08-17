import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { installCommand, TOOLS } from './tools';
import { STABLE_SET } from './site';

/**
 * Why: the flagship pages assert things about crates. The two claims most
 * likely to rot silently are the ones the reader will act on — the `-p` flag
 * they paste into `cargo test`, and the install command — because both look
 * plausible while being wrong. `crates/trusty-git-analytics` is package `tga`,
 * so the directory name is NOT the package name and cannot be assumed.
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

	it('every tctl install command targets a stable-set member', () => {
		const tctl = TOOLS.filter((tool) => tool.install.via === 'tctl');
		expect(tctl.length).toBe(TOOLS.length - 1);
		for (const tool of tctl) {
			const target = tool.install.via === 'tctl' ? tool.install.target : '';
			expect(STABLE_SET, `${tool.name} installs ${target}`).toContain(target);
			// The rendered block is the bootstrap line, then the tctl line.
			expect(installCommand(tool)).toContain(`\ntctl install ${target}`);
		}
	});

	/**
	 * The one tool tctl does not manage. `trusty-audit` is `publish = false` and
	 * absent from `stable_set.rs`, so a `tctl install` line on its page would be
	 * a command that cannot work. Its own bootstrap script is shipped by #5873;
	 * this asserts the URL SHAPE rather than the file's presence, because the
	 * script lands in a different PR from this page.
	 */
	it('installs trusty-audit from its own bootstrap script, not tctl', () => {
		const audit = TOOLS.find((tool) => tool.name === 'trusty-audit')!;
		expect(audit.install.via).toBe('script');
		expect(installCommand(audit)).toBe(
			'curl -fsSL https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/crates/trusty-audit/install.sh | sh'
		);
		expect(STABLE_SET).not.toContain('trusty-audit');
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
			'search_code',
			// The `taudit` alias still exists in the binary and is being dropped.
			// No user-facing string may name it: `trusty-audit` everywhere.
			'taudit'
		]) {
			expect(prose, banned).not.toContain(banned);
		}
	});
});
