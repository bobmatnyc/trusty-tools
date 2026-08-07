import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

/**
 * Why: the token-parity suite is a plain Node test over two CSS files, while
 * the theme store needs a DOM. Running both under one environment would
 * either load jsdom for nothing or leave `document` undefined for the store.
 * What: two Vitest projects — `unit` (jsdom, `src/**`) and `smoke` (node,
 * `tests/**`, long timeout because it shells out to a real `vite build`).
 */
export default defineConfig({
	plugins: [sveltekit()],
	test: {
		projects: [
			{
				extends: true,
				test: {
					name: 'unit',
					environment: 'jsdom',
					include: ['src/**/*.test.ts']
				}
			},
			{
				extends: true,
				test: {
					name: 'smoke',
					environment: 'node',
					include: ['tests/**/*.test.ts'],
					testTimeout: 300_000,
					hookTimeout: 300_000
				}
			}
		]
	}
});
