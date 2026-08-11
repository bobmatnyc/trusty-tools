import { describe, expect, it } from 'vitest';

import { parsePage, renderPage } from './render';
import type { LinkContext } from './links';

/**
 * Why: the published pages are real engineering documentation — GFM tables,
 * fenced blocks with language hints, nested lists — so the renderer is checked
 * against those constructs rather than a synthetic paragraph. The two hardening
 * gates get their own cases because both prevent silent content loss.
 * What: parse-then-render with a fixture link context.
 * Test: this file.
 */

const SHA = 'b'.repeat(40);

const ctx = (overrides: Partial<LinkContext> = {}): LinkContext => ({
	fromSource: 'docs/page.md',
	bySource: new Map(),
	probe: () => 'missing',
	commitSha: SHA,
	headingIds: () => new Set(),
	...overrides
});

const render = (markdown: string, overrides?: Partial<LinkContext>) => {
	const parsed = parsePage(markdown);
	return { parsed, ...renderPage(parsed, ctx(overrides)) };
};

describe('markdown constructs the real pages use', () => {
	it('renders a GFM table, wrapped so the page never scrolls sideways', () => {
		const { html } = render('| A | B |\n| --- | --- |\n| 1 | 2 |');
		expect(html).toContain('<div class="doc-table">');
		expect(html).toContain('<thead>');
		expect(html).toContain('<td>1</td>');
	});

	it('keeps a fenced block’s language hint and lifts it onto the pre', () => {
		const { html } = render('```bash\ntm ls\n```');
		expect(html).toContain('<pre data-lang="bash">');
		expect(html).toContain('<code class="language-bash">');
	});

	it('renders a fence with no language hint', () => {
		const { html } = render('```\nplain\n```');
		expect(html).toContain('<pre><code>plain');
	});

	it('renders nested lists', () => {
		const { html } = render('- one\n  - nested\n- two');
		expect(html).toContain('<ul>');
		expect(html.match(/<ul>/g)).toHaveLength(2);
	});

	it('renders GFM strikethrough, task lists, and reference-style links', () => {
		const { html } = render('~~gone~~\n\n- [x] done\n\n[ref][r]\n\n[r]: https://example.com');
		expect(html).toContain('<del>gone</del>');
		expect(html).toContain('type="checkbox"');
		expect(html).toContain('href="https://example.com"');
	});
});

describe('headings', () => {
	it('assigns ids and builds a table of contents from h2/h3 only', () => {
		const { parsed } = render('# Title\n\n## First Section\n\n### Detail\n\n#### Deep');
		expect(parsed.headingIds).toEqual(new Set(['title', 'first-section', 'detail', 'deep']));
		expect(parsed.toc).toEqual([
			{ id: 'first-section', text: 'First Section', depth: 2 },
			{ id: 'detail', text: 'Detail', depth: 3 }
		]);
	});

	it('disambiguates repeated headings the way GitHub does', () => {
		const { parsed } = render('## Notes\n\n## Notes');
		expect(parsed.toc.map((entry) => entry.id)).toEqual(['notes', 'notes-1']);
	});
});

describe('link rewriting inside the tree', () => {
	it('marks a repository permalink as external and counts it', () => {
		const { html, counts, failures } = render('[spec](specs/foo.md)', {
			probe: (relative) => (relative === 'docs/specs/foo.md' ? 'file' : 'missing')
		});
		expect(failures).toEqual([]);
		expect(counts['repo-file']).toBe(1);
		expect(html).toContain(
			`href="https://github.com/bobmatnyc/trusty-tools/blob/${SHA}/docs/specs/foo.md"`
		);
		expect(html).toContain('rel="noreferrer noopener"');
		expect(html).toContain('class="doc-link-external"');
	});

	it('reports the line of a broken link and keeps going', () => {
		const { failures } = render('intro\n\n[a](gone.md)\n\n[b](also-gone.md)');
		expect(failures.map((f) => f.code)).toEqual(['BROKEN-LINK', 'BROKEN-LINK']);
		expect(failures[0].line).toBe(3);
		expect(failures[1].line).toBe(5);
	});
});

describe('hardening gates', () => {
	// `<crate>` in prose parses as an unknown element and renders as NOTHING.
	// This corpus writes angle-bracket metavariables constantly.
	it('rejects an angle-bracket metavariable written outside a code span', () => {
		const { failures } = render('Run tctl install <crate> now.');
		expect(failures[0].code).toBe('RAW-HTML');
		expect(failures[0].problem).toContain('<crate>');
		expect(failures[0].remedy).toContain('backticks');
	});

	it('accepts the same metavariable inside a code span', () => {
		const { html, failures } = render('Run `tctl install <crate>` now.');
		expect(failures).toEqual([]);
		expect(html).toContain('&#x3C;crate>');
	});

	it('accepts genuine inline HTML that markdown pages legitimately use', () => {
		const { failures } = render('<details><summary>More</summary>\n\ntext\n\n</details>');
		expect(failures).toEqual([]);
	});

	// The published site must issue no third-party requests at all.
	it('rejects an image the site does not serve itself', () => {
		const { failures } = render('![chart](../assets/chart.png)');
		expect(failures[0].code).toBe('OFF-SITE-IMAGE');
		expect(failures[0].remedy).toContain('website/static/');
	});

	it('accepts an image served from this site', () => {
		const { failures } = render('![mark](/favicon.svg)');
		expect(failures).toEqual([]);
	});
});
