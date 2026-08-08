import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { CRATE_GROUPS, FACTS, FLAGSHIPS, GITHUB_URL, INSTALL_OPTIONS, STABLE_SET } from './site';

/**
 * Why: a landing page is where people copy-paste with the least scepticism, so
 * every command shown has to work. The first version of this suite checked the
 * commands against root `README.md`, which turned out to be the wrong contract
 * — the README undercounts the Homebrew tap, omits a supported platform, and
 * agrees with a self-labelled draft that was excluded from publication. Prose
 * docs cannot be the authority for an executable claim.
 *
 * What: grounds each claim in the repository itself — the bootstrap URL
 * resolves to a tracked file, `cargo install --path` names a directory that
 * exists, `tctl` names members that appear in `stable_set.rs`, the MSRV
 * matches the workspace `rust-version`, and the platform list matches the
 * Tier-1 triples in `platform.rs`.
 *
 * Not covered here, deliberately: whether the Homebrew tap's assets download.
 * That needs a network call, which does not belong in a unit suite. It was
 * verified by hand on 2026-08-07 — all ten formulae in `bobmatnyc/homebrew-trusty`
 * returned HTTP 200 for their darwin-arm64 asset. What this file CAN pin is
 * that the tap is named consistently and the formula is fully qualified.
 *
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../..');

const read = (rel: string) => readFileSync(path.join(REPO_ROOT, rel), 'utf8');
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

	it('never tells a reader to build without installing', () => {
		// `cargo build --release` produces a binary in target/ and installs
		// nothing. The step a reader improvises next is copying it onto their
		// PATH, which is the macOS cdhash trap CLAUDE.md warns about.
		for (const c of commands) {
			expect(c).not.toContain('cargo build');
			expect(c).not.toMatch(/\b(cp|mv)\s+target\//);
		}
	});

	it('STABLE_SET matches stable_set.rs', () => {
		const source = read('crates/trusty-installer/src/commands/stable_set.rs');
		const declared = [...source.matchAll(/StableMember::new\("([^"]+)"/g)].map((m) => m[1]);
		expect(declared.length).toBe(7);
		expect(STABLE_SET).toEqual(declared);
	});

	it('any crate named in a tctl command is a stable-set member', () => {
		for (const c of commands.filter((c) => c.startsWith('tctl install'))) {
			for (const member of c.replace('tctl install', '').trim().split(/\s+/).filter(Boolean)) {
				expect(STABLE_SET, member).toContain(member);
			}
		}
	});

	it('the Homebrew formula is fully qualified against the tap it taps', () => {
		const tap = commands.find((c) => c.startsWith('brew tap'))!.split(/\s+/)[2];
		const install = commands.find((c) => c.startsWith('brew install'))!.split(/\s+/)[2];
		expect(tap).toBe('bobmatnyc/trusty');
		// `brew install <user>/<tap>/<formula>` — unambiguous even if a
		// same-named formula ever lands in homebrew-core.
		expect(install.startsWith(`${tap}/`)).toBe(true);
	});
});

describe('stated facts match their source of truth', () => {
	it('MSRV matches the workspace rust-version', () => {
		// Anchored to line start: the `[workspace.package]` comment block above
		// the real declaration quotes `rust-version = "1.94.1"` (the AWS SDK's
		// floor, not this workspace's), and an unanchored match reads that.
		const declared = read('Cargo.toml').match(/^rust-version\s*=\s*"([^"]+)"/m)![1];
		const shown = FACTS.find((f) => f.label === 'MSRV')!.value.replace('Rust ', '');
		expect(shown).toBe(declared);
	});

	it('the platform list matches the Tier-1 triples', () => {
		const source = read('crates/trusty-installer/src/download/platform.rs');
		const shown = FACTS.find((f) => f.label === 'Prebuilt for')!.value;
		// Root README.md lists only two of these three.
		for (const [label, triple] of [
			['macOS arm64', 'aarch64-apple-darwin'],
			['Linux x86_64', 'x86_64-unknown-linux-gnu'],
			['Linux arm64', 'aarch64-unknown-linux-gnu']
		]) {
			expect(source, triple).toContain(triple);
			expect(shown, label).toContain(label);
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
