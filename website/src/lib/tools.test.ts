import { describe, expect, it } from 'vitest';
import { installCommand, TOOLS } from './tools';
import { STABLE_SET } from './site';

/**
 * Why: the flagship pages assert things about crates. The two claims most
 * likely to rot silently are the ones the reader will act on — the `-p` flag
 * they paste into `cargo test`, and the install command — because both look
 * plausible while being wrong. A crate's directory name is not guaranteed to
 * be its package name and cannot be assumed.
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
		// #8507: tga and trusty-audit moved to their own site, and every
		// remaining flagship installs via tctl.
		expect(tctl.length).toBe(TOOLS.length);
		for (const tool of tctl) {
			const target = tool.install.via === 'tctl' ? tool.install.target : '';
			expect(STABLE_SET, `${tool.name} installs ${target}`).toContain(target);
			// The rendered block is the bootstrap line, then the tctl line.
			expect(installCommand(tool)).toContain(`\ntctl install ${target}`);
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
