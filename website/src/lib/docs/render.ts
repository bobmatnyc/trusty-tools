/**
 * Why: the published pages are real engineering documentation — GFM tables,
 * fenced blocks with language hints, nested lists, footnote-style reference
 * links — so the renderer has to be a full CommonMark+GFM pipeline, not a
 * regex. It also has to run entirely at BUILD time: the output is inlined into
 * prerendered HTML, and the shipped page loads no markdown and calls nothing.
 *
 * What: two phases, deliberately separate because link rewriting needs
 * knowledge the first phase produces.
 *
 *   `parsePage`  markdown → hast, with heading ids and a table of contents.
 *   `renderPage` rewrites every link and image against `links.ts`, hardens the
 *                tree, and stringifies.
 *
 * The split exists because `other.md#section` can only be validated once the
 * TARGET page's heading ids are known — so every page is parsed before any page
 * is rendered (`site.ts` sequences it).
 *
 * Syntax highlighting is deliberately absent. A highlighter's token palette
 * would need its own light/dark WCAG audit on top of the Foundry tokens; a
 * fenced block instead renders as one high-contrast foreground on the raised
 * surface (13.29:1 light, 11.88:1 dark) with its language hint shown as a
 * label. Readability, at no contrast risk.
 *
 * Test: `render.test.ts` — tables, fenced blocks, nested lists, heading ids and
 * TOC, the code-block language label, and the two hardening gates.
 */

import rehypeRaw from 'rehype-raw';
import rehypeSlug from 'rehype-slug';
import rehypeStringify from 'rehype-stringify';
import remarkGfm from 'remark-gfm';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import { unified } from 'unified';
import { visit } from 'unist-util-visit';
import { toString as hastToString } from 'hast-util-to-string';
import type { Element, Root } from 'hast';

import type { DocFailure } from './errors';
import { resolveLink, type LinkClass, type LinkContext } from './links';

export interface TocEntry {
	id: string;
	text: string;
	/** Heading level, 2–3; `h1` is the page title and is not listed. */
	depth: number;
}

export interface ParsedPage {
	tree: Root;
	toc: TocEntry[];
	headingIds: Set<string>;
}

export interface RenderedPage {
	html: string;
	failures: DocFailure[];
	/** How many links landed in each class. Reported by the build. */
	counts: Record<LinkClass, number>;
}

const HEADINGS = new Set(['h1', 'h2', 'h3', 'h4', 'h5', 'h6']);

/**
 * Why: `<crate>` written in prose instead of `` `<crate>` `` parses as an
 * unknown HTML element and renders as NOTHING — the text silently vanishes
 * from the published page. This corpus uses angle-bracket metavariables
 * constantly, so that is a live content-loss hazard, not a hypothetical.
 * What: every element name a markdown page may legitimately produce or embed.
 * Anything else fails the build.
 * Test: `render.test.ts` (`rejects an angle-bracket metavariable written
 * outside a code span`).
 */
const ALLOWED_ELEMENTS = new Set([
	'a',
	'abbr',
	'b',
	'blockquote',
	'br',
	'caption',
	'code',
	'col',
	'colgroup',
	'dd',
	'del',
	'details',
	'div',
	'dl',
	'dt',
	'em',
	'figcaption',
	'figure',
	'h1',
	'h2',
	'h3',
	'h4',
	'h5',
	'h6',
	'hr',
	'i',
	'img',
	'input',
	'ins',
	'kbd',
	'li',
	'mark',
	'ol',
	'p',
	'picture',
	'pre',
	'q',
	's',
	'samp',
	'section',
	'small',
	'source',
	'span',
	'strong',
	'sub',
	'summary',
	'sup',
	'table',
	'tbody',
	'td',
	'tfoot',
	'th',
	'thead',
	'tr',
	'u',
	'ul',
	'var'
]);

const parser = unified()
	.use(remarkParse)
	.use(remarkGfm)
	.use(remarkRehype, { allowDangerousHtml: true })
	.use(rehypeRaw)
	.use(rehypeSlug);

const stringifier = unified().use(rehypeStringify);

/**
 * Why: phase one — see the module header.
 * What: parses one markdown source into hast, assigning a slug to every
 * heading and collecting the ids and TOC the second phase and the page
 * component need.
 * Test: `render.test.ts` (`assigns heading ids and builds a table of contents`).
 */
export function parsePage(markdown: string): ParsedPage {
	const tree = parser.runSync(parser.parse(markdown)) as Root;
	const headingIds = new Set<string>();
	const toc: TocEntry[] = [];

	visit(tree, 'element', (node: Element) => {
		if (!HEADINGS.has(node.tagName)) return;
		const id = typeof node.properties?.id === 'string' ? node.properties.id : '';
		if (id === '') return;
		headingIds.add(id);
		const depth = Number(node.tagName.slice(1));
		if (depth >= 2 && depth <= 3) toc.push({ id, text: hastToString(node), depth });
	});

	return { tree, toc, headingIds };
}

/**
 * Why: phase two — every link classified, nothing rendered that a reader
 * cannot act on, and no element that would silently swallow its own text.
 * What: mutates `parsed.tree` in place (each page is rendered once), then
 * serialises it. Failures ACCUMULATE so one build reports every broken link.
 * Test: `render.test.ts`, and the classification cases in `links.test.ts`.
 */
export function renderPage(parsed: ParsedPage, ctx: LinkContext): RenderedPage {
	const failures: DocFailure[] = [];
	const counts: Record<LinkClass, number> = {
		external: 0,
		anchor: 0,
		site: 0,
		'repo-file': 0,
		'repo-dir': 0
	};
	const wrapped = new WeakSet<Element>();

	visit(parsed.tree, 'element', (node: Element, index, parent) => {
		if (!ALLOWED_ELEMENTS.has(node.tagName)) {
			failures.push({
				code: 'RAW-HTML',
				file: ctx.fromSource,
				line: node.position?.start.line,
				problem: `\`<${node.tagName}>\` is not an HTML element, so everything inside it renders as nothing`,
				remedy: `wrap it in backticks (\`\\\`<${node.tagName}>\\\`\`) if it is a placeholder, or escape the \`<\``
			});
			return;
		}

		if (node.tagName === 'a') {
			rewriteAnchor(node, ctx, failures, counts);
			return;
		}

		if (node.tagName === 'img') {
			checkImage(node, ctx, failures);
			return;
		}

		if (node.tagName === 'pre') {
			labelCodeBlock(node);
			return;
		}

		// Wide tables must scroll inside their own container rather than making
		// the page scroll sideways. The WeakSet stops the visitor re-wrapping
		// the table it just descended into.
		if (node.tagName === 'table' && parent && typeof index === 'number' && !wrapped.has(node)) {
			wrapped.add(node);
			parent.children[index] = {
				type: 'element',
				tagName: 'div',
				properties: { className: ['doc-table'] },
				children: [node]
			};
		}
	});

	return { html: stringifier.stringify(parsed.tree), failures, counts };
}

function rewriteAnchor(
	node: Element,
	ctx: LinkContext,
	failures: DocFailure[],
	counts: Record<LinkClass, number>
): void {
	const href = node.properties?.href;
	if (typeof href !== 'string') return;

	const outcome = resolveLink(href, { ...ctx, line: node.position?.start.line });
	if ('failure' in outcome) {
		failures.push(outcome.failure);
		return;
	}

	const { link } = outcome;
	counts[link.class] += 1;
	node.properties = { ...node.properties, href: link.href };
	if (link.external) {
		node.properties.rel = ['noreferrer', 'noopener'];
		node.properties.className = ['doc-link-external'];
	}
}

/**
 * Why: the published site makes NO runtime connections — not to a CDN, not to
 * raw.githubusercontent.com, not to anything. An image referenced by relative
 * path or by an off-site URL is therefore either broken or a third-party
 * request, and both are defects rather than things to render.
 */
function checkImage(node: Element, ctx: LinkContext, failures: DocFailure[]): void {
	const src = node.properties?.src;
	if (typeof src !== 'string') return;
	if (src.startsWith('/')) return;
	failures.push({
		code: 'OFF-SITE-IMAGE',
		file: ctx.fromSource,
		line: node.position?.start.line,
		problem: `image \`${src}\` is not served by this site`,
		remedy:
			'copy the asset into `website/static/` and reference it as `/<name>` — the published site must issue no third-party requests'
	});
}

/** Lifts a fenced block's language hint onto the `<pre>` so CSS can label it. */
function labelCodeBlock(node: Element): void {
	const code = node.children.find(
		(child): child is Element => child.type === 'element' && child.tagName === 'code'
	) as Element | undefined;
	const classes = code?.properties?.className;
	if (!Array.isArray(classes)) return;
	const language = classes
		.map(String)
		.find((name) => name.startsWith('language-'))
		?.slice('language-'.length);
	if (language) node.properties = { ...node.properties, 'data-lang': language };
}
