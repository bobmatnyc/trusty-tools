import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

/**
 * Why: `src/app.css` HAND-TRANSCRIBES the canonical Foundry hex values from
 * `docs/design/UI/design-system/tokens.css` into Tailwind's RGB-triple form.
 * A hand-edit on either side drifts them apart silently — the site keeps
 * building and simply renders a colour the design system never approved.
 * `scripts/check_token_drift.mjs` is the repo-wide gate for exactly this, but
 * `website/` is not registered there yet (that entry belongs to PR #5103), so
 * without this suite the theme layer would ship with no proof at all.
 *
 * What: re-derives every mapped `--color-*` from the canonical `--trusty-*`
 * hex and compares, per theme. Also pins the contrast facts the layout
 * depends on, because those are the reason the site uses `text-secondary`
 * where the design system would reach for `text-muted`.
 *
 * Test: this file. Run with `pnpm test`.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '../../..');
const REPO_ROOT = path.resolve(WEBSITE_ROOT, '..');

const CANONICAL = readFileSync(
	path.join(REPO_ROOT, 'docs/design/UI/design-system/tokens.css'),
	'utf8'
);
const APP_CSS = readFileSync(path.join(WEBSITE_ROOT, 'src/app.css'), 'utf8');

/**
 * The registration `scripts/check_token_drift.mjs` will carry for this
 * package. Kept identical to the entry named in `app.css`'s header so this
 * suite fails if the file is restructured in a way that entry could not parse.
 */
const CANONICAL_LIGHT = /:root\s*\{([\s\S]*?)\n\}/;
const CANONICAL_DARK = /\[data-theme='dark'\], \.dark\s*\{([\s\S]*?)\n\}/;
const SITE_LIGHT = /:root\s*\{([\s\S]*?)\n\}/;
const SITE_DARK = /\.dark\s*\{([\s\S]*?)\n\}/;

/** `--color-*` suffix in app.css → `--trusty-*` suffix in tokens.css. */
const MAPPINGS: [string, string][] = [
	['content-bg', 'content-bg'],
	['card-bg', 'card-bg'],
	['surface-raised', 'surface-raised'],
	['border', 'border'],
	['border-strong', 'border-strong'],
	['text-primary', 'text-primary'],
	['text-secondary', 'text-secondary'],
	['text-muted', 'text-muted'],
	['text-inverse', 'text-inverse'],
	['primary', 'accent'],
	['primary-hover', 'accent-hover'],
	['success', 'success'],
	['warning', 'warning'],
	['danger', 'danger'],
	['info', 'info'],
	['sidebar-bg', 'sidebar-bg'],
	['sidebar-text', 'sidebar-text'],
	['sidebar-muted', 'sidebar-muted'],
	['sidebar-accent', 'sidebar-accent'],
	['sidebar-border', 'sidebar-border']
];

/** Carried over verbatim — bakes its own alpha, so it is not triple-mapped. */
const PASSTHROUGH = ['trusty-surface-hover'];

function block(source: string, selector: RegExp, label: string): string {
	const match = source.match(selector);
	if (!match) throw new Error(`no ${label} block matched ${selector}`);
	// The canonical dark selector has two groups; the block is always last.
	return match[match.length - 1];
}

function readToken(blockBody: string, name: string): string {
	const match = blockBody.match(new RegExp(`--${name}\\s*:\\s*([^;]+);`));
	if (!match) throw new Error(`token --${name} not found`);
	return match[1].trim();
}

function hexToTriple(hex: string): string {
	const m = hex.trim().match(/^#([0-9a-f]{6})$/i);
	if (!m) throw new Error(`not a 6-digit hex: ${hex}`);
	const v = m[1];
	return [0, 2, 4].map((i) => parseInt(v.slice(i, i + 2), 16)).join(' ');
}

const canonicalLight = block(CANONICAL, CANONICAL_LIGHT, 'canonical light');
const canonicalDark = block(CANONICAL, CANONICAL_DARK, 'canonical dark');
const siteLight = block(APP_CSS, SITE_LIGHT, 'site light');
const siteDark = block(APP_CSS, SITE_DARK, 'site dark');

const THEMES: [string, string, string][] = [
	['light', canonicalLight, siteLight],
	['dark', canonicalDark, siteDark]
];

describe('app.css matches the canonical Foundry tokens', () => {
	it('parses a non-empty block for every theme on both sides', () => {
		for (const [name, canonical, site] of THEMES) {
			expect(canonical.length, `canonical ${name}`).toBeGreaterThan(0);
			expect(site.length, `site ${name}`).toBeGreaterThan(0);
		}
		// Guards against a regex that matches an empty or wrong block and
		// makes every comparison below vacuously pass.
		expect(MAPPINGS.length).toBeGreaterThan(15);
	});

	for (const [themeName, canonicalBlock, siteBlock] of THEMES) {
		describe(themeName, () => {
			it.each(MAPPINGS)('--color-%s tracks --trusty-%s', (colorName, trustyName) => {
				const expected = hexToTriple(readToken(canonicalBlock, `trusty-${trustyName}`));
				expect(readToken(siteBlock, `color-${colorName}`)).toBe(expected);
			});

			it.each(PASSTHROUGH)('%s is carried over verbatim', (name) => {
				const normalize = (s: string) => s.replace(/\s+/g, ' ').toLowerCase();
				expect(normalize(readToken(siteBlock, name))).toBe(
					normalize(readToken(canonicalBlock, name))
				);
			});
		});
	}

	it('actually differs between themes', () => {
		// A copy-paste that left both blocks identical would pass every
		// comparison above only if tokens.css were also identical — it is not,
		// but assert the end result directly rather than trusting that.
		expect(readToken(siteLight, 'color-primary')).not.toBe(readToken(siteDark, 'color-primary'));
		expect(readToken(siteLight, 'color-content-bg')).not.toBe(
			readToken(siteDark, 'color-content-bg')
		);
	});
});

/**
 * Why: WCAG AA in both themes is a stated requirement of this site, and two
 * canonical tokens do NOT clear it for normal-size text — light
 * `--trusty-text-muted` (3.87:1) and light `--trusty-accent` on
 * `--trusty-surface-raised` (4.50:1, just under). The layout works around
 * both. These assertions pin the numbers so a future token revision that
 * fixes or worsens them shows up here instead of in a manual audit.
 */
describe('contrast facts the layout depends on', () => {
	const relativeLuminance = (hex: string) =>
		[1, 3, 5]
			.map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
			.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4))
			.reduce((acc, c, i) => acc + [0.2126, 0.7152, 0.0722][i] * c, 0);

	const ratio = (a: string, b: string) => {
		const [hi, lo] = [relativeLuminance(a), relativeLuminance(b)].sort((x, y) => y - x);
		return (hi + 0.05) / (lo + 0.05);
	};

	const light = (name: string) => readToken(canonicalLight, `trusty-${name}`);
	const dark = (name: string) => readToken(canonicalDark, `trusty-${name}`);

	it('body text clears AA on both grounds', () => {
		expect(ratio(light('text-primary'), light('content-bg'))).toBeGreaterThanOrEqual(4.5);
		expect(ratio(light('text-secondary'), light('content-bg'))).toBeGreaterThanOrEqual(4.5);
		expect(ratio(dark('text-primary'), dark('content-bg'))).toBeGreaterThanOrEqual(4.5);
		expect(ratio(dark('text-secondary'), dark('content-bg'))).toBeGreaterThanOrEqual(4.5);
	});

	it('primary button labels clear AA in both themes', () => {
		expect(ratio(light('text-inverse'), light('accent'))).toBeGreaterThanOrEqual(4.5);
		expect(ratio(dark('text-inverse'), dark('accent'))).toBeGreaterThanOrEqual(4.5);
	});

	it('the accent clears the 3:1 non-text minimum for focus rings', () => {
		expect(ratio(light('accent'), light('content-bg'))).toBeGreaterThanOrEqual(3);
		expect(ratio(dark('accent'), dark('content-bg'))).toBeGreaterThanOrEqual(3);
	});

	it('light text-muted still fails AA for normal text', () => {
		// The reason `.eyebrow` and every small label use text-secondary.
		// If this ever starts passing, the workaround can be dropped.
		expect(ratio(light('text-muted'), light('content-bg'))).toBeLessThan(4.5);
		expect(ratio(dark('text-muted'), dark('content-bg'))).toBeGreaterThanOrEqual(4.5);
	});

	it('light accent on the raised surface still fails AA for normal text', () => {
		// The reason the raised crate-lineup band carries no accent-coloured
		// body text.
		expect(ratio(light('accent'), light('surface-raised'))).toBeLessThan(4.5);
	});

	/**
	 * Why: `.badge-red` / `.badge-amber` / `.badge-green` in `app.css` are
	 * written asymmetrically — red and green colour the LABEL, amber colours
	 * only its BORDER — and the reason is a contrast measurement that lived
	 * solely in a comment. A later edit to `--trusty-danger`, `--trusty-success`
	 * or `--trusty-warning` in the canonical tokens would silently falsify it
	 * (code-critic on #5415). These derive the ratios from the tokens and
	 * assert the threshold each rule actually depends on.
	 */
	describe('the tga audit severity badges', () => {
		// The stamps sit in a table on the page ground and, for the plain
		// `.badge`, on a card — so both grounds are asserted.
		const grounds = ['content-bg', 'card-bg'];

		it('red and green badge labels clear AA as normal text', () => {
			// 11px text, so 4.5:1 — not the 3:1 non-text minimum.
			for (const token of ['danger', 'success']) {
				for (const ground of grounds) {
					expect(ratio(light(token), light(ground))).toBeGreaterThanOrEqual(4.5);
					expect(ratio(dark(token), dark(ground))).toBeGreaterThanOrEqual(4.5);
				}
			}
		});

		it('the amber badge border clears the 3:1 non-text minimum', () => {
			// `.badge-amber` puts warning on the border only, so WCAG 1.4.11
			// governs it rather than the 4.5:1 text rule.
			expect(ratio(light('warning'), light('content-bg'))).toBeGreaterThanOrEqual(3);
			expect(ratio(dark('warning'), dark('content-bg'))).toBeGreaterThanOrEqual(3);
		});

		it('light warning still fails AA, which is why amber colours no label', () => {
			// If this ever starts passing, `.badge-amber` can carry its colour
			// in the text like the other two and the asymmetry can be dropped.
			expect(ratio(light('warning'), light('content-bg'))).toBeLessThan(4.5);
			expect(ratio(dark('warning'), dark('content-bg'))).toBeGreaterThanOrEqual(4.5);
		});

		it('the amber badge label uses text-primary, which clears AA', () => {
			expect(ratio(light('text-primary'), light('content-bg'))).toBeGreaterThanOrEqual(4.5);
			expect(ratio(dark('text-primary'), dark('content-bg'))).toBeGreaterThanOrEqual(4.5);
		});
	});
});
