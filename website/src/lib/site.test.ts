import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { CRATE_GROUPS, FACTS, FLAGSHIPS, GITHUB_URL, INSTALL_OPTIONS } from './site';

/**
 * Why: the landing page's factual claims are only as good as their source. The
 * install commands and the MSRV are the two that actively rot — a README edit
 * or an MSRV bump leaves the site confidently wrong, and nothing else in the
 * build would notice. Crate names are checked against `crates/` for the same
 * reason: a rename would otherwise leave a dead name on the page.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../..');
const README = readFileSync(path.join(REPO_ROOT, 'README.md'), 'utf8');

describe('landing-page content is sourced from the repository', () => {
	it.each(INSTALL_OPTIONS.map((o) => [o.id, o.command] as const))(
		'the %s command appears in README.md',
		(_id, command) => {
			for (const line of command.split('\n')) {
				expect(README).toContain(line);
			}
		}
	);

	it('states the MSRV the README states', () => {
		const msrv = FACTS.find((f) => f.label === 'MSRV');
		expect(msrv).toBeDefined();
		const version = msrv!.value.replace('Rust ', '');
		expect(README).toContain(`MSRV:** Rust ${version}`);
	});

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

	it('carries no placeholder text', () => {
		const prose = JSON.stringify({ FLAGSHIPS, CRATE_GROUPS, INSTALL_OPTIONS, FACTS });
		for (const banned of ['lorem', 'ipsum', 'TODO', 'TBD', 'coming soon', 'placeholder']) {
			expect(prose.toLowerCase()).not.toContain(banned.toLowerCase());
		}
	});

	it('points at the canonical repository', () => {
		expect(GITHUB_URL).toBe('https://github.com/bobmatnyc/trusty-tools');
	});
});
