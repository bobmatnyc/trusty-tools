import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';

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

beforeAll(() => {
	rmSync(OUTPUT, { recursive: true, force: true });
	execFileSync('node', [path.join(WEBSITE_ROOT, 'node_modules/vite/bin/vite.js'), 'build'], {
		cwd: WEBSITE_ROOT,
		stdio: 'inherit'
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

	// The published site must be fully self-contained: everything is baked at
	// build time and the page opens no connection to any service. Anchor hrefs
	// are destinations a reader clicks, not requests — only subresources count.
	it('loads no subresource from a third-party origin', () => {
		for (const name of ['index.html', 'docs.html', ...docPages()]) {
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
		expect(landingPage).toContain('Three flagship MCP servers');
		expect(landingPage).toContain('brew tap bobmatnyc/trusty');
	});

	it('sets the theme class before first paint', () => {
		// The anti-flash snippet must survive the build inlined in the HTML.
		expect(landingPage).toContain("classList.toggle('dark'");
	});
});
