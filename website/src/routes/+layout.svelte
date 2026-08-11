<script lang="ts">
	import { dev } from '$app/environment';
	import { injectAnalytics } from '@vercel/analytics/sveltekit';
	import '../app.css';
	import SiteHeader from '$lib/components/SiteHeader.svelte';
	import SiteFooter from '$lib/components/SiteFooter.svelte';

	let { children } = $props();

	/**
	 * Why: the site publishes to Vercel and the deploy had no traffic signal at
	 * all. Web Analytics is the one Vercel provides without a cookie banner or a
	 * third-party origin — in `production` mode the script is served first-party
	 * from `/_vercel/insights/script.js`, so the self-contained property the
	 * build smoke test guards survives (`tests/build-smoke.test.ts`).
	 * What: registers the client-side pageview beacon. `injectAnalytics` returns
	 * early unless `$app/environment`'s `browser` is true, so prerendering emits
	 * no script tag and the beacon attaches at hydration instead.
	 *
	 * `mode` picks WHICH script loads, not whether one loads. A dev server still
	 * gets a tag: `development` selects Vercel's debug script on
	 * `va.vercel-scripts.com`, which console-logs events and records nothing, and
	 * is the only third-party origin this package ever reaches for. Passing `dev`
	 * is therefore what keeps that origin out of the deployed bundle. Leaving
	 * `mode` at its default `auto` would instead infer the environment from
	 * `process.env.NODE_ENV` inside the browser bundle — do not.
	 * Test: `tests/build-smoke.test.ts`, "wires analytics to a first-party path,
	 * never the third-party debug script".
	 */
	injectAnalytics({ mode: dev ? 'development' : 'production' });
</script>

<!-- Keyboard users land here first; the target must be focusable, hence tabindex. -->
<a
	href="#main"
	class="sr-only rounded border-[1.5px] border-foundry-primary bg-foundry-card px-4 py-2 focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-50"
>
	Skip to content
</a>

<div class="flex min-h-screen flex-col">
	<SiteHeader />
	<main id="main" tabindex="-1" class="flex-1 focus:outline-none">
		{@render children()}
	</main>
	<SiteFooter />
</div>
