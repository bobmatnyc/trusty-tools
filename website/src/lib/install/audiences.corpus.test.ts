import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { AUDIENCES, PREREQUISITES, type CommandBlock } from './audiences';

/**
 * Why: a published `cargo install <package>` line that names no real package
 * fails in the reader's terminal, and the only authority on the package name is
 * each crate's own manifest — `crates/trusty-git-analytics` is package `tga`.
 * Proving it walks every `crates/*\/Cargo.toml`, which the `unit` project does
 * not run for, so this is a CORPUS case (#8272).
 * What: builds the real package-name set from `crates/**` and checks every
 * rendered `cargo install` line against it. Every other audience case needs no
 * filesystem and stays in `audiences.test.ts`.
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

describe('every command is one the repository can actually run', () => {
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
});
