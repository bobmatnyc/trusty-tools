import { existsSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { CRATE_GROUPS, FLAGSHIPS, INSTALL_OPTIONS } from './site';

/**
 * Why: three landing-page claims are grounded in files OUTSIDE `website/` — the
 * bootstrap script at the repo root, the crate directories a `cargo install
 * --path` names, and the crate list the page prints. A change to any of them
 * lands under `crates/**`, which the `unit` project does not run for, so these
 * belong to the CORPUS job (#8272). The MSRV and installer-source cases stay in
 * `site.test.ts`: root `Cargo.toml` and the two `crates/trusty-installer`
 * sources are exact unit triggers.
 * What: existence checks against the real checkout, and one `crates/` listing.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../..');

const commands = INSTALL_OPTIONS.flatMap((o) => o.command.split('\n'));

describe('install commands are grounded in the repository', () => {
	it('the bootstrap URL points at a file that exists at the repo root', () => {
		const line = commands.find((c) => c.includes('install.sh'));
		expect(line, 'no bootstrap command present').toBeDefined();

		const url = line!.match(/https:\/\/\S+install\.sh/)![0];
		const prefix = `https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/`;
		expect(url.startsWith(prefix)).toBe(true);
		expect(existsSync(path.join(REPO_ROOT, url.slice(prefix.length)))).toBe(true);
	});

	it('every cargo install --path names a crate that exists', () => {
		const paths = commands
			.filter((c) => c.includes('cargo install --path'))
			.map((c) => c.match(/--path\s+(\S+)/)![1]);
		expect(paths.length).toBeGreaterThan(0);
		for (const p of paths) {
			expect(existsSync(path.join(REPO_ROOT, p, 'Cargo.toml')), p).toBe(true);
		}
	});
});

describe('landing-page content', () => {
	it('names only crates that exist', () => {
		const onDisk = new Set(
			readdirSync(path.join(REPO_ROOT, 'crates'), { withFileTypes: true })
				.filter((e) => e.isDirectory())
				.map((e) => e.name)
		);
		const named = [
			...FLAGSHIPS.map((f) => f.name),
			...CRATE_GROUPS.flatMap((g) => g.crates.map((c) => c.name))
		];
		expect(named.length).toBeGreaterThan(10);
		for (const name of named) {
			expect(onDisk, `crates/${name}`).toContain(name);
		}
	});
});
