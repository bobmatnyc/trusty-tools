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
 * What: the record checks that need no filesystem. Nothing here reads the
 * repository: the cases that re-derive a crate directory, package name, release
 * state, docs route, page source or MCP tool count are `tools.corpus.test.ts`
 * (#8272).
 *
 * Not covered here: the page copy itself. Prose claims were verified by hand
 * against each crate's clap enums and MCP descriptor tables; a unit test
 * cannot re-derive an English sentence.
 *
 * Test: this file.
 */

describe('flagship tool records are grounded in the repository', () => {
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
