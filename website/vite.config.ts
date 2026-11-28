import adapter from '@sveltejs/adapter-vercel';
import { sveltekit } from '@sveltejs/kit/vite';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

/**
 * Why: the public marketing/docs site (#5097) deploys to Vercel from the
 * `website/` subdirectory of this Rust workspace, so the adapter — not a
 * static export — is what Vercel's build container expects to find.
 * What: SvelteKit config, passed to the `sveltekit()` plugin because
 * SvelteKit 3 no longer reads `svelte.config.js`. `adapter-vercel` emits
 * `.vercel/output/` in the Build Output API v3 layout; every route here is
 * prerendered (`src/routes/+layout.ts`), so in practice the adapter writes
 * static HTML and provisions no serverless function. No `runtime` is passed on
 * purpose: adapter-vercel then derives `nodejs<major>.x` from the Node that
 * runs the build, which is the Vercel project's "Node.js Version" setting. A
 * pinned `nodejs20.x` (#5117) broke every deploy once Vercel discontinued it
 * (2026-10-01); following the project setting survives the next retirement.
 * Test: `pnpm build` in `tests/build-smoke.test.ts` asserts the prerendered
 * `index.html` lands in `.vercel/output/static/`.
 *
 * The doc reader (#5098) attaches at `src/routes/docs/` — see
 * `src/lib/docs/README.md` for the seam contract.
 */
const sveltekitConfig = {
	preprocess: vitePreprocess(),
	adapter: adapter(),
	alias: {
		// SvelteKit 3 no longer generates `$lib`; declaring it here keeps every
		// existing `$lib/...` import resolving. `alias` is deprecated in favour
		// of `#lib` subpath imports — moving to those is a separate change.
		$lib: 'src/lib',
		// The doc reader (#5098) reads `docs/public-manifest.tsv` and the
		// Markdown sources it points at, both of which live OUTSIDE
		// `website/`. This alias is the single declared path to them, so
		// the "Include source files outside of the Root Directory" Vercel
		// setting has exactly one thing to satisfy (see README.md).
		$repo: '../'
	}
};

/**
 * Why: the token-parity suite is a plain Node test over two CSS files, while
 * the theme store needs a DOM. Running both under one environment would
 * either load jsdom for nothing or leave `document` undefined for the store.
 * What: three Vitest projects — `unit` (jsdom, `src/**`, and nothing in it
 * reads the repository), `corpus` (node, `src/**\/*.corpus.test.ts`, every
 * suite that DOES read real repository content) and `smoke` (node,
 * `tests/**`, long timeout because it shells out to a real `vite build`).
 * `smoke` also disables file parallelism: more than one `tests/**` file now
 * shells out to `vite build` into the same fixed `.vercel/output`
 * (build-smoke.test.ts, mobile-overflow.test.ts), and `adapter-vercel`
 * errors EEXIST symlinking a function's `node_modules` if a second build
 * starts before the first's is torn down — each file's own `beforeAll`
 * still clears that directory, which is only safe run in turn.
 */
/**
 * Hook AND test budget for the `corpus` project — every suite that reads real
 * repository content: the six-crate changelog corpus, the 27-page docs corpus,
 * the flagship pages, and the crate-grounded landing-page claims.
 *
 * MEASURED 2026-09-18 on this repo at 8df2360932: one `buildChangelogSite()`
 * over the corpus is ~49_000 ms under `unit`'s jsdom environment and
 * ~21_300 ms under this project's node environment. The `beforeAll` builds it
 * once and reuses it, so ~21_300 ms is the hook's cost. 300_000 ms is >14x
 * that — well past the 5x rule and the 60_000 ms floor — with headroom for a
 * hosted runner and for a corpus that grows every release.
 *
 * `hookTimeout` is the half that matters and the half that was missing: it
 * defaults to 10_000 ms and does NOT follow `testTimeout`, so the 120 s
 * `testTimeout` below never covered `site.test.ts`'s `beforeAll`, which is how
 * PR #8272 — a Rust-only fix — went red on "Website tests".
 */
const CORPUS_TIMEOUT_MS = 300_000;

/**
 * Hook AND test budget for the `smoke` project, whose every file shells out to
 * a real `vite build`.
 *
 * MEASURED 1m21s (81_000 ms) unloaded. OBSERVED 2026-09-18 on a machine also
 * running `cargo` builds: the same build took 4m15s (255_000 ms), 3.1x the
 * unloaded figure, and the previous `300_000 ms` — only 3.7x the measurement —
 * left 45 s of headroom against a hosted runner that is slower again. 900_000 ms
 * is >11x the measurement and >3.5x the loaded observation.
 *
 * `build-smoke.test.ts` and `mobile-overflow.test.ts` each run their OWN build,
 * by design: Vitest guarantees no file ordering, so neither may assume the
 * other's `.vercel/output` is present or current. `fileParallelism: false`
 * below is what keeps the two builds from racing on that one fixed path.
 */
const SMOKE_TIMEOUT_MS = 900_000;

export default defineConfig({
	plugins: [sveltekit(sveltekitConfig)],
	test: {
		projects: [
			{
				extends: true,
				// #5110: the install walkthrough's test mounts a real component,
				// and `mount()` only exists in Svelte's CLIENT build. Vitest
				// resolves packages under the `ssr` conditions by default even in
				// a jsdom environment, which hands the test `svelte/index-server`
				// and fails with `mount(...) is not available on the server`. This
				// is the resolution SvelteKit's own testing guidance prescribes,
				// scoped to this project so the `smoke` project — which shells out
				// to a real `vite build` — keeps the build's own conditions.
				resolve: { conditions: ['browser'] },
				test: {
					name: 'unit',
					environment: 'jsdom',
					include: ['src/**/*.test.ts'],
					// No test in this project may walk the repository: the
					// real-changelog cases live in the `corpus` project below and
					// report as their own CI check. Excluding them here is what
					// keeps a changelog fragment or a docs edit off this suite.
					exclude: ['src/**/*.corpus.test.ts'],
					// #5200: several suites read repo files (the docs manifest,
					// Cargo.toml, the token CSS) under vitest's 5s default, which
					// a hosted runner is materially slower at than a laptop.
					testTimeout: 120_000,
					// `hookTimeout` does NOT follow `testTimeout` — it stays at
					// vitest's 10s default until set. That gap is half the defect
					// behind PR #8272's red; the other half was three suites here
					// building a real corpus in `beforeAll`. All of them are in
					// the `corpus` project now, so nothing in this project reads
					// the repository — but the budget stays, because a hosted
					// runner is still slow at the small file reads that remain.
					hookTimeout: 120_000
				}
			},
			{
				extends: true,
				test: {
					// The real-corpus changelog gate, split out of `unit` so a
					// changelog fragment stops paying for the whole website unit
					// suite and an 11k-line parse stops running under jsdom for
					// nothing — node is ~2.3x faster on it (see CORPUS_TIMEOUT_MS).
					name: 'corpus',
					environment: 'node',
					include: ['src/**/*.corpus.test.ts'],
					testTimeout: CORPUS_TIMEOUT_MS,
					hookTimeout: CORPUS_TIMEOUT_MS
				}
			},
			{
				extends: true,
				test: {
					name: 'smoke',
					environment: 'node',
					include: ['tests/**/*.test.ts'],
					testTimeout: SMOKE_TIMEOUT_MS,
					hookTimeout: SMOKE_TIMEOUT_MS,
					fileParallelism: false
				}
			}
		]
	}
});
