import adapter from '@sveltejs/adapter-vercel';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/**
 * Why: the public marketing/docs site (#5097) deploys to Vercel from the
 * `website/` subdirectory of this Rust workspace, so the adapter — not a
 * static export — is what Vercel's build container expects to find.
 * What: SvelteKit config. `adapter-vercel` emits `.vercel/output/` in the
 * Build Output API v3 layout; every route here is prerendered
 * (`src/routes/+layout.ts`), so in practice the adapter writes static HTML
 * and provisions no serverless function. The Node runtime is pinned so a
 * Vercel default-runtime bump cannot silently change the build.
 * Test: `pnpm build` in `tests/build-smoke.test.ts` asserts the prerendered
 * `index.html` lands in `.vercel/output/static/`.
 *
 * The doc reader (#5098) attaches at `src/routes/docs/` — see
 * `src/lib/docs/README.md` for the seam contract.
 */
export default {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({ runtime: 'nodejs20.x' }),
		alias: {
			// The doc reader (#5098) reads `docs/public-manifest.tsv` and the
			// Markdown sources it points at, both of which live OUTSIDE
			// `website/`. This alias is the single declared path to them, so
			// the "Include source files outside of the Root Directory" Vercel
			// setting has exactly one thing to satisfy (see README.md).
			$repo: '../'
		}
	}
};
