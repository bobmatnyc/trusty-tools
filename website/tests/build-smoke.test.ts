import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';
import { TOOLS } from '../src/lib/tools';

/**
 * Why: every other test here reads source files. None of them prove the site
 * actually BUILDS, and a Vercel deploy failure is the expensive way to find
 * that out. This runs the real production build once and asserts the artefacts
 * a Vercel deploy depends on — the Build Output API layout, the prerendered
 * landing page, and the self-hosted fonts.
 * What: shells out to `vite build`, then inspects `.vercel/output/`. Slow
 * (tens of seconds), which is why it lives in its own `smoke` Vitest project
 * with a long timeout rather than alongside the unit tests.
 * Test: this file.
 */

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(HERE, '..');
const REPO_ROOT = path.resolve(WEBSITE_ROOT, '..');
const OUTPUT = path.join(WEBSITE_ROOT, '.vercel/output');
const STATIC = path.join(OUTPUT, 'static');

let landingPage = '';

/** Every `/docs/...` route the manifest declares, `/` included. */
function manifestRoutes(): string[] {
	return readFileSync(path.join(REPO_ROOT, 'docs/public-manifest.tsv'), 'utf8')
		.split('\n')
		.filter((line) => line.startsWith('PAGE\t'))
		.map((line) => line.split('\t')[3]);
}

/** `/` → `docs.html`, `/a/b` → `docs/a/b.html`. */
function routeToArtifact(route: string): string {
	return route === '/' ? 'docs.html' : `docs${route}.html`;
}

/** The Build Output API v3 config the adapter emitted. */
function vercelConfig(): { overrides: Record<string, { path: string }>; routes: unknown[] } {
	return JSON.parse(readFileSync(path.join(OUTPUT, 'config.json'), 'utf8'));
}

/**
 * The hand-authored `/tools/<slug>` pages, as emitted static files.
 *
 * `docPages()` below is manifest-driven, so it cannot see these — they are
 * routes, not documentation. Without this the self-containment assertion
 * would silently skip every flagship page.
 */
function toolPages(): string[] {
	return TOOLS.map((tool) => `tools/${tool.slug}.html`);
}

/** Prerendered doc artifacts on disk, as paths relative to the static root. */
function docPages(): Set<string> {
	const found = new Set<string>();
	if (existsSync(path.join(STATIC, 'docs.html'))) found.add('docs.html');
	const walk = (dir: string, prefix: string) => {
		if (!existsSync(dir)) return;
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const next = `${prefix}/${entry.name}`;
			if (entry.isDirectory()) walk(path.join(dir, entry.name), next);
			else if (entry.name.endsWith('.html')) found.add(next.slice(1));
		}
	};
	walk(path.join(STATIC, 'docs'), '/docs');
	return found;
}

/** Every built client-side JS chunk, concatenated. */
function clientBundle(): string {
	const chunks: string[] = [];
	const walk = (dir: string) => {
		if (!existsSync(dir)) return;
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const full = path.join(dir, entry.name);
			if (entry.isDirectory()) walk(full);
			else if (entry.name.endsWith('.js')) chunks.push(readFileSync(full, 'utf8'));
		}
	};
	walk(path.join(STATIC, '_app'));
	return chunks.join('\n');
}

beforeAll(() => {
	rmSync(OUTPUT, { recursive: true, force: true });
	execFileSync('node', [path.join(WEBSITE_ROOT, 'node_modules/vite/bin/vite.js'), 'build'], {
		cwd: WEBSITE_ROOT,
		stdio: 'inherit',
		// Vitest exports NODE_ENV=test, which this subprocess inherits, and Vite
		// derives `isProduction` from `process.env.NODE_ENV || mode`. Left alone,
		// `import.meta.env.DEV` — and therefore SvelteKit's `dev` — comes out TRUE
		// in a `vite build`, so this suite validated an artifact Vercel never
		// produces. Harmless until something branched on `dev`; the analytics
		// wiring in `+layout.svelte` does, and picks the third-party debug script
		// when it reads true. Pinned so the build under test is the deployed one.
		env: { ...process.env, NODE_ENV: 'production' }
	});
	landingPage = readFileSync(path.join(STATIC, 'index.html'), 'utf8');
});

describe('production build', () => {
	it('emits the Vercel Build Output API layout', () => {
		expect(existsSync(path.join(OUTPUT, 'config.json'))).toBe(true);
		expect(existsSync(STATIC)).toBe(true);
	});

	it('prerenders the landing page and the docs root to static HTML', () => {
		expect(existsSync(path.join(STATIC, 'index.html'))).toBe(true);
		expect(existsSync(path.join(STATIC, 'docs.html'))).toBe(true);
	});

	it('prerenders exactly the manifest, one file per PAGE row', () => {
		expect(docPages().size).toBe(manifestRoutes().length);
		for (const route of manifestRoutes()) {
			expect(docPages().has(routeToArtifact(route))).toBe(true);
		}
	});

	// THE BOUNDARY, at the artifact level: an unlisted docs/ file produces no
	// output and no route entry, so nothing serves it.
	it('emits no artifact and no route for an unlisted docs/ file', () => {
		const adr = readdirSync(path.join(REPO_ROOT, 'docs/adr')).find((n) => n.endsWith('.md'))!;
		const slug = adr.replace(/\.md$/, '');
		expect(existsSync(path.join(REPO_ROOT, 'docs/adr', adr))).toBe(true);
		expect(existsSync(path.join(STATIC, 'docs/adr', `${slug}.html`))).toBe(false);
		expect([...docPages()].some((name) => name.includes('adr'))).toBe(false);
		expect(JSON.stringify(vercelConfig())).not.toContain('adr');
	});

	// `adapter-vercel` always provisions a catchall function, so the guarantee
	// that matters is that no PUBLISHED route depends on it: each one is an
	// override the CDN serves from disk.
	it('serves every manifest route from a static file, not the catchall function', () => {
		const overrides = Object.values(vercelConfig().overrides).map((o) => `/${o.path}`);
		for (const route of manifestRoutes()) {
			expect(overrides).toContain(route === '/' ? '/docs' : `/docs${route}`);
		}
	});

	// The published site loads everything it renders from its own origin:
	// content, CSS, fonts and scripts are all baked at build time. Anchor hrefs
	// are destinations a reader clicks, not requests — only subresources count.
	//
	// Vercel Web Analytics (#5097) is the one service the page now talks to, and
	// it does not appear here: `injectAnalytics` no-ops unless `browser`, so
	// prerendering emits no tag, and in `production` mode the beacon it appends
	// at hydration is the FIRST-PARTY `/_vercel/insights/script.js`. That is why
	// this list is still empty rather than carrying an exception — the assertion
	// below is deliberately unchanged. The runtime half is pinned by
	// 'wires analytics to a first-party path…' underneath.
	it('prerenders every flagship tool page to static HTML', () => {
		for (const name of toolPages()) {
			expect(existsSync(path.join(STATIC, name)), name).toBe(true);
		}
	});

	// Why: the assertion above proves only that a FILE exists. A page whose
	// component threw during prerender, or one wired to the wrong `Tool` record,
	// still writes an HTML shell and still passes it — and with six hand-authored
	// pages, a copy/paste that leaves two routes rendering the same crate is the
	// realistic failure, not a missing file. Nothing else in this suite reads a
	// tool page's body.
	// What: for each `TOOLS` entry, re-derives what that page must contain from
	// the same record the page renders from, and pins it to the route's own
	// artifact — the `<h1>` must name THAT crate, and the unit stamp, tagline,
	// lede, install command, and docs link must all be present.
	it('renders each flagship page from its own tool record, not a shell', () => {
		for (const tool of TOOLS) {
			const file = `tools/${tool.slug}.html`;
			const html = readFileSync(path.join(STATIC, file), 'utf8');

			const heading = html.match(/<h1\b[^>]*>([\s\S]*?)<\/h1>/);
			expect(heading, `${file} has no <h1>`).not.toBeNull();
			expect(heading?.[1], `${file} <h1>`).toContain(tool.name);

			for (const copy of [tool.unit, tool.tagline, tool.lede, tool.install]) {
				expect(html, `${file} is missing ${JSON.stringify(copy)}`).toContain(copy);
			}
			if (tool.docsPath) {
				expect(html, `${file} does not link ${tool.docsPath}`).toContain(`href="${tool.docsPath}"`);
			}
		}
	});

	it('loads no subresource from a third-party origin', () => {
		for (const name of ['index.html', 'docs.html', ...docPages(), ...toolPages()]) {
			const html = readFileSync(path.join(STATIC, name), 'utf8');
			const subresources = [
				...html.matchAll(/<(?:script|img|source|iframe)\b[^>]*\bsrc="([^"]+)"/g),
				...html.matchAll(/<link\b[^>]*\bhref="([^"]+)"/g)
			].map((match) => match[1]);
			const offSite = subresources.filter((url) => /^(?:https?:)?\/\//.test(url));
			expect(offSite, `${name} loads ${offSite.join(', ')}`).toEqual([]);
			expect(html).not.toContain('@import url(http');
		}
	});

	// The HTML walk above cannot see this: the analytics beacon is appended by
	// client JS after hydration, so the only evidence is the bundle. `mode`
	// decides the origin — `production` selects Vercel's first-party proxy path,
	// `development` selects va.vercel-scripts.com. Dropping the `mode` argument,
	// or a build where SvelteKit's `dev` reads true, silently ships the
	// third-party one, and nothing else in this file would notice.
	it('wires analytics to a first-party path, never the third-party debug script', () => {
		const bundle = clientBundle();
		expect(bundle).toContain('/_vercel/insights/script.js');
		expect(bundle).toMatch(/\{\s*mode:\s*["']production["']\s*\}/);
		expect(bundle).not.toMatch(/\{\s*mode:\s*["']development["']\s*\}/);
	});

	it('renders real documentation, with rewritten links and no blob/main', () => {
		const intro = readFileSync(path.join(STATIC, 'docs.html'), 'utf8');
		expect(intro).toContain('doc-prose');
		expect(intro).toContain('trusty-tools documentation');
		for (const name of docPages()) {
			const html = readFileSync(path.join(STATIC, name), 'utf8');
			expect(html.includes('/blob/main/'), `${name} links to blob/main`).toBe(false);
			// A relative `.md` href means a link the rewriter missed. Permalinks
			// legitimately end in `.md`, so only non-absolute ones are a defect.
			const unrewritten = [...html.matchAll(/href=\\?"((?!https?:|#|\/)[^"\\]*\.md)/g)];
			expect(
				unrewritten.map((m) => m[1]),
				`${name} has unrewritten links`
			).toEqual([]);
		}
	});

	it('ships the self-hosted fonts and references no external font host', () => {
		expect(existsSync(path.join(STATIC, 'fonts/ibm-plex-sans-var.woff2'))).toBe(true);
		expect(existsSync(path.join(STATIC, 'fonts/OFL-IBMPlexSans.txt'))).toBe(true);
		expect(landingPage).not.toContain('fonts.googleapis.com');
		expect(landingPage).not.toContain('fonts.gstatic.com');
	});

	it('renders real landing-page content, not a shell', () => {
		expect(landingPage).toContain('trusty-search');
		expect(landingPage).toContain('Six flagship tools');
		expect(landingPage).toContain('brew tap bobmatnyc/trusty');
		for (const tool of TOOLS) {
			expect(landingPage, tool.slug).toContain(`/tools/${tool.slug}`);
		}
	});

	it('sets the theme class before first paint', () => {
		// The anti-flash snippet must survive the build inlined in the HTML.
		expect(landingPage).toContain("classList.toggle('dark'");
	});
});
