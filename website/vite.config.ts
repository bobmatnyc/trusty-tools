import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

/**
 * Why: the token-parity suite is a plain Node test over two CSS files, while
 * the theme store needs a DOM. Running both under one environment would
 * either load jsdom for nothing or leave `document` undefined for the store.
 * What: two Vitest projects — `unit` (jsdom, `src/**`) and `smoke` (node,
 * `tests/**`, long timeout because it shells out to a real `vite build`).
 * `smoke` also disables file parallelism: more than one `tests/**` file now
 * shells out to `vite build` into the same fixed `.vercel/output`
 * (build-smoke.test.ts, mobile-overflow.test.ts), and `adapter-vercel`
 * errors EEXIST symlinking a function's `node_modules` if a second build
 * starts before the first's is torn down — each file's own `beforeAll`
 * still clears that directory, which is only safe run in turn.
 */
export default defineConfig({
	plugins: [sveltekit()],
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
					// #5200: the changelog tests parse the real six-crate corpus
					// (11k lines and growing every release) under vitest's 5s
					// default. Worst case measured 21.1s locally; a hosted runner
					// is materially slower on CPU-bound parsing.
					testTimeout: 120_000
				}
			},
			{
				extends: true,
				test: {
					name: 'smoke',
					environment: 'node',
					include: ['tests/**/*.test.ts'],
					testTimeout: 300_000,
					hookTimeout: 300_000,
					fileParallelism: false
				}
			}
		]
	}
});
