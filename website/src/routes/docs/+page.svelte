<script lang="ts">
	import { GITHUB_URL } from '$lib/site';

	/**
	 * Placeholder. The doc reader (#5098) replaces this file wholesale — see
	 * `src/lib/docs/README.md` for the seam contract. It exists so the `/docs`
	 * nav link resolves instead of 404-ing, and so the scaffold's route
	 * structure is visible to whoever picks up #5098.
	 */
	const DOCS_URL = `${GITHUB_URL}/tree/main/docs`;
</script>

<svelte:head>
	<title>Documentation — trusty-tools</title>
	<meta name="description" content="Documentation for the trusty-tools Rust workspace." />
</svelte:head>

<section class="mx-auto max-w-content px-4 py-16 sm:px-6 sm:py-24">
	<p class="eyebrow">Documentation</p>
	<h1 class="mt-4 font-display text-3xl font-bold tracking-tight sm:text-4xl">
		Not published here yet
	</h1>
	<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
		The hosted documentation reader is still being built. Until it lands, the full documentation
		tree is readable in the repository.
	</p>

	<div class="mt-8 flex flex-wrap gap-3">
		<a href={DOCS_URL} rel="noreferrer noopener" class="btn btn-primary">Read docs on GitHub</a>
		<a href="/" class="btn btn-secondary">Back to home</a>
	</div>
</section>
