import { readFileSync } from 'node:fs';
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
 * What: grounds each claim in a file the `unit` project is triggered by — `tctl`
 * names members that appear in `stable_set.rs`, the MSRV matches the workspace
 * `rust-version` in root `Cargo.toml`, and the platform list matches the Tier-1
 * triples in `platform.rs`. The claims resting on `crates/**` directories and on
 * the root bootstrap script are `site.corpus.test.ts` (#8272).
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
		// #5200: scope the match to the `stable_set()` body. Sweeping the whole
		// file also counts the `StableMember::new` in the test module — a
		// fixture, not a shipped member — so the count drifted with test edits.
		const body = source.match(/pub fn stable_set\(\) -> Vec<StableMember> \{([\s\S]*?)\n\}/);
		expect(body, 'stable_set() not found in stable_set.rs').not.toBeNull();
		const declared = [...body![1].matchAll(/StableMember::new\("([^"]+)"/g)].map((m) => m[1]);
		expect(declared.length).toBe(8);
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
