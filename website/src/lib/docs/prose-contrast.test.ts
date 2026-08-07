import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

import { findRepoRoot } from './repo';

/**
 * Why: three canonical Foundry tokens do not clear WCAG AA and the landing page
 * routes around them. The doc reader adds a lot of new surface — code blocks,
 * tables, a sidebar, an on-this-page rail — and each one is a chance to
 * reintroduce a pair the landing page already rejected. This recomputes the
 * ratios from `app.css` itself, so a future palette revision fails here rather
 * than shipping.
 * What: every foreground/background pair the `.doc-prose` rules actually use,
 * checked in both themes, plus a structural check that the three known-failing
 * pairs appear nowhere in the doc CSS.
 * Test: this file. `src/lib/theme/tokens.test.ts` separately pins app.css
 * against the canonical `docs/design/UI/design-system/tokens.css`.
 */

const APP_CSS = readFileSync(path.join(findRepoRoot(), 'website/src/app.css'), 'utf8');

function palette(selector: RegExp): Record<string, [number, number, number]> {
	const block = APP_CSS.match(selector);
	if (!block) throw new Error(`no block matched ${selector}`);
	const out: Record<string, [number, number, number]> = {};
	for (const [, name, triple] of block[1].matchAll(/--color-([\w-]+):\s*([\d\s]+);/g)) {
		const parts = triple.trim().split(/\s+/).map(Number);
		out[name] = [parts[0], parts[1], parts[2]];
	}
	return out;
}

const LIGHT = palette(/:root\s*\{([\s\S]*?)\n\}/);
const DARK = palette(/\.dark\s*\{([\s\S]*?)\n\}/);

const channel = (value: number) => {
	const c = value / 255;
	return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};

const luminance = ([r, g, b]: [number, number, number]) =>
	0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);

function ratio(theme: Record<string, [number, number, number]>, fg: string, bg: string): number {
	const a = luminance(theme[fg]);
	const b = luminance(theme[bg]);
	return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

/** Every pair the `.doc-prose` rules put on screen, and where it is used. */
const PAIRS: { fg: string; bg: string; where: string }[] = [
	{ fg: 'text-primary', bg: 'content-bg', where: 'body copy, headings, table cells' },
	{ fg: 'text-primary', bg: 'surface-raised', where: 'fenced code blocks' },
	{ fg: 'text-primary', bg: 'card-bg', where: 'inline code, table head' },
	{ fg: 'text-secondary', bg: 'content-bg', where: 'blockquote, sidebar, on-this-page' },
	{ fg: 'text-secondary', bg: 'surface-raised', where: 'the code block’s language label' },
	{ fg: 'text-secondary', bg: 'card-bg', where: 'mobile nav disclosure' },
	{ fg: 'primary', bg: 'content-bg', where: 'links in body copy and table cells' },
	{ fg: 'primary', bg: 'card-bg', where: 'links inside inline code and in the table head' }
];

/** The pairs the landing page already established as failing. Never reintroduce. */
const KNOWN_FAILING: { fg: string; bg: string; ratio: number }[] = [
	{ fg: 'text-muted', bg: 'content-bg', ratio: 3.87 },
	{ fg: 'primary', bg: 'surface-raised', ratio: 4.5 },
	{ fg: 'warning', bg: 'content-bg', ratio: 3.18 }
];

const DOC_CSS = APP_CSS.slice(APP_CSS.indexOf('/* Documentation prose (#5098)'));

describe('documentation prose contrast', () => {
	it.each(PAIRS)('$fg on $bg clears AA in both themes ($where)', ({ fg, bg }) => {
		expect(ratio(LIGHT, fg, bg)).toBeGreaterThanOrEqual(4.5);
		expect(ratio(DARK, fg, bg)).toBeGreaterThanOrEqual(4.5);
	});

	it('still measures the three known-failing pairs as failing, so the routing-around is warranted', () => {
		for (const pair of KNOWN_FAILING) {
			expect(ratio(LIGHT, pair.fg, pair.bg)).toBeCloseTo(pair.ratio, 1);
			expect(ratio(LIGHT, pair.fg, pair.bg)).toBeLessThan(4.5);
		}
	});

	it('uses neither the muted nor the warning token anywhere in the doc rules', () => {
		expect(DOC_CSS).not.toMatch(/foundry-muted/);
		expect(DOC_CSS).not.toMatch(/foundry-warning/);
	});

	// primary-on-raised is 4.50:1, so a link must never sit on the raised
	// surface. The only raised background in the doc rules is the <pre>, and a
	// fenced code block cannot contain a link.
	it('puts the raised surface only under a fenced code block', () => {
		const raisedRules = DOC_CSS.split('}')
			.filter((rule) => rule.includes('bg-foundry-raised'))
			.map((rule) => rule.split('{')[0].trim());
		expect(raisedRules).toEqual(['.doc-prose pre']);
	});

	it('sets no colour on inline code, so it stays link-coloured inside a link', () => {
		const inlineCode = DOC_CSS.match(/\.doc-prose :not\(pre\) > code \{[^}]*\}/)?.[0] ?? '';
		expect(inlineCode).toContain('bg-foundry-card');
		expect(inlineCode).not.toMatch(/text-foundry-/);
	});
});
