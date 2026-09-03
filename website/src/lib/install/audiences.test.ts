import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
	AUDIENCES,
	audienceText,
	hasPlaceholder,
	PREREQUISITES,
	prerequisite,
	usedPrerequisites,
	type CommandBlock
} from './audiences';
import { STABLE_SET } from '../site';

/**
 * Why: a published install command that does not work is the worst failure
 * this site has, and prose review does not catch it — `tctl install
 * trusty-code` reads exactly like the seven lines above it and fails with an
 * unknown-member error. The macOS permission is the same class of defect with
 * a worse blast radius: telling a reader to grant `tm` the disk-wide category
 * would be a security regression, not a typo (#5110).
 *
 * What: re-derives every install target from the repository — crate
 * directories, package names in their own `Cargo.toml`, and `tctl` stable-set
 * membership — and pins the two permission invariants the research doc
 * establishes.
 *
 * Not covered here: whether a sentence reads well. This asserts the claims a
 * reader will paste into a terminal or a system-settings pane.
 *
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, '../../../..');

/** Package name from a crate's own manifest — `trusty-git-analytics` is `tga`. */
function packageNames(): Set<string> {
	const cratesDir = path.join(REPO_ROOT, 'crates');
	const names = new Set<string>();
	for (const entry of readdirSync(cratesDir, { withFileTypes: true })) {
		if (!entry.isDirectory()) continue;
		const manifest = path.join(cratesDir, entry.name, 'Cargo.toml');
		if (!existsSync(manifest)) continue;
		// Anchored: a `[dependencies]` entry further down also matches `name`.
		const declared = readFileSync(manifest, 'utf8').match(/^name\s*=\s*"([^"]+)"/m);
		if (declared) names.add(declared[1]);
	}
	return names;
}

/** Every command block the page can render, audience commands and shared setup. */
function allCommands(): CommandBlock[] {
	return [
		...PREREQUISITES.flatMap((p) => p.commands),
		...AUDIENCES.flatMap((a) => a.steps.flatMap((s) => s.commands))
	];
}

describe('the nine install audiences', () => {
	it('covers the nine paths the research doc establishes, with unique ids', () => {
		expect(AUDIENCES.length).toBe(9);
		expect(new Set(AUDIENCES.map((a) => a.id)).size).toBe(9);
		expect(AUDIENCES.map((a) => a.id)).toEqual([
			'trusty-memory',
			'trusty-search',
			'search-and-memory',
			'trusty-analyze',
			'trusty-review',
			'trusty-mpm',
			'trusty-code',
			'trusty-agents',
			'tga'
		]);
	});

	it('gives every audience at least one ordered step', () => {
		for (const audience of AUDIENCES) {
			expect(audience.steps.length, audience.id).toBeGreaterThan(0);
			expect(audience.prerequisites.length, audience.id).toBeGreaterThan(0);
		}
	});

	it('resolves every prerequisite reference, and renders each one once', () => {
		for (const audience of AUDIENCES) {
			for (const ref of audience.prerequisites) {
				expect(() => prerequisite(ref.id), `${audience.id} → ${ref.id}`).not.toThrow();
			}
			// A prerequisite named twice by one audience would render twice in
			// its roster, which is the duplication this module exists to avoid.
			const ids = audience.prerequisites.map((r) => r.id);
			expect(new Set(ids).size, audience.id).toBe(ids.length);
		}
		// The shared section renders exactly the prerequisites something asks
		// for — a row nothing references is dead copy on a public page.
		expect(usedPrerequisites().length).toBe(PREREQUISITES.length);
	});
});

describe('every command is one the repository can actually run', () => {
	it('names a stable-set member on every tctl install line', () => {
		const targets = allCommands()
			.flatMap((block) => block.command.split('\n'))
			.filter((line) => line.startsWith('tctl install '))
			.flatMap((line) => line.slice('tctl install '.length).split(' '));
		expect(targets.length).toBeGreaterThan(0);
		for (const target of targets) {
			expect(STABLE_SET, `tctl install ${target}`).toContain(target);
		}
	});

	/**
	 * The two products `tctl` cannot install. `stable_set.rs` has seven entries
	 * and neither is among them, so a `tctl install` line on either audience is
	 * a command that fails with `unknown member(s)`.
	 */
	it('offers no tctl install line to trusty-code or trusty-agents', () => {
		for (const id of ['trusty-code', 'trusty-agents']) {
			const audience = AUDIENCES.find((a) => a.id === id)!;
			for (const step of audience.steps) {
				for (const block of step.commands) {
					expect(block.command, `${id} step "${step.title}"`).not.toContain('tctl install');
				}
			}
			expect(STABLE_SET).not.toContain(id);
		}
	});

	it('names a real package on every cargo install line', () => {
		const packages = packageNames();
		const named = allCommands()
			.flatMap((block) => block.command.split('\n'))
			.map((line) => line.match(/^cargo install ([a-z0-9-]+) --locked$/))
			.filter((match): match is RegExpMatchArray => match !== null)
			.map((match) => match[1]);
		expect(named.length).toBeGreaterThan(0);
		for (const name of named) {
			expect(packages, `cargo install ${name}`).toContain(name);
		}
	});

	it('names a crate directory that exists on every cargo install --path line', () => {
		const paths = allCommands()
			.flatMap((block) => block.command.split('\n'))
			.map((line) => line.match(/^cargo install --path (\S+) --locked$/))
			.filter((match): match is RegExpMatchArray => match !== null)
			.map((match) => match[1]);
		expect(paths).toEqual(['crates/trusty-agents']);
		for (const dir of paths) {
			expect(existsSync(path.join(REPO_ROOT, dir, 'Cargo.toml')), dir).toBe(true);
		}
	});

	it('gives every command block a distinct, non-empty copy label', () => {
		const labels = allCommands().map((block) => block.label);
		for (const label of labels) {
			expect(label.length).toBeGreaterThan(0);
		}
		expect(new Set(labels).size).toBe(labels.length);
	});
});

describe('placeholders always arrive with the instruction that replaces them', () => {
	it('carries a note naming each placeholder it prints', () => {
		const withPlaceholders = allCommands().filter((block) => hasPlaceholder(block.command));
		// Not a vacuous pass: the provider-key exports genuinely print one.
		expect(withPlaceholders.length).toBeGreaterThan(0);
		for (const block of withPlaceholders) {
			expect(block.placeholderNote, block.command).toBeTruthy();
			for (const placeholder of block.command.match(/<[^<>\s]+>/g) ?? []) {
				expect(block.placeholderNote, `${block.command} → ${placeholder}`).toContain(placeholder);
			}
		}
	});

	it('leaves no note on a command that has nothing to substitute', () => {
		for (const block of allCommands()) {
			if (hasPlaceholder(block.command)) continue;
			expect(block.placeholderNote, block.command).toBeUndefined();
		}
	});
});

describe('macOS permission categories are per product and never widened', () => {
	it('asks for Full Disk Access only where trusty-search is involved', () => {
		const fda = AUDIENCES.filter((a) => a.tcc.category === 'full-disk-access').map((a) => a.id);
		expect(fda).toEqual(['trusty-search', 'search-and-memory']);
	});

	/**
	 * The invariant this whole page was gated on. `tm` and `tagent` read other
	 * apps' `$HOME` containers, which raises the SEPARATE "access data from
	 * other apps" prompt; the disk-wide category is a different, wider grant
	 * that neither should ever be offered. The doc is explicit that it "should
	 * never be granted" to `tm`, so its name may not appear in that audience's
	 * copy at all — not even as a warning a reader could skim into an
	 * instruction.
	 */
	it('never prints the wider category anywhere in an App Data audience', () => {
		const appData = AUDIENCES.filter((a) => a.tcc.category === 'app-data').map((a) => a.id);
		expect(appData).toEqual(['trusty-mpm', 'trusty-agents']);
		for (const id of appData) {
			const audience = AUDIENCES.find((a) => a.id === id)!;
			expect(audienceText(audience), id).not.toContain('Full Disk Access');
			expect(audienceText(audience), id).toContain('App Data');
		}
	});

	it('says so explicitly where no permission applies', () => {
		const none = AUDIENCES.filter((a) => a.tcc.category === 'none');
		expect(none.map((a) => a.id)).toEqual([
			'trusty-memory',
			'trusty-analyze',
			'trusty-review',
			'trusty-code',
			'tga'
		]);
		for (const audience of none) {
			expect(audience.tcc.summary, audience.id).not.toContain('Full Disk Access');
			expect(audience.tcc.summary.length, audience.id).toBeGreaterThan(0);
		}
	});
});

describe('the MCP registration each audience publishes', () => {
	/**
	 * Argument vectors, verbatim from the research doc's per-audience sections.
	 * `trusty-review` is the one with two accepted spellings; `["mcp"]` is the
	 * canonical one to publish and `["serve", "--stdio"]` survives only as a
	 * back-compat alias, so the canonical form is what the block prints.
	 */
	const EXPECTED: Record<string, string> = {
		'trusty-memory': '"args": ["serve","--stdio"]',
		'trusty-search': '"args": ["serve"]',
		'trusty-analyze': '"args": ["serve","--mcp"]',
		'trusty-review': '"args": ["mcp"]',
		'trusty-agents': '"args": ["mcp-serve"]'
	};

	it('prints the argument vector the crate actually parses', () => {
		for (const [id, args] of Object.entries(EXPECTED)) {
			const audience = AUDIENCES.find((a) => a.id === id)!;
			const commands = audience.steps.flatMap((s) => s.commands.map((c) => c.command));
			expect(
				commands.some((c) => c.includes(args)),
				`${id} MCP args`
			).toBe(true);
		}
	});

	it('registers nothing for tga, which has no MCP transport', () => {
		const tga = AUDIENCES.find((a) => a.id === 'tga')!;
		for (const step of tga.steps) {
			for (const block of step.commands) {
				expect(block.command, 'tga').not.toContain('mcpServers');
			}
		}
		expect(audienceText(tga)).toContain('no MCP transport');
	});
});
