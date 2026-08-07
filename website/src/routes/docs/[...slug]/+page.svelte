<script lang="ts">
	/**
	 * Why: the markdown is already HTML by the time it reaches here — parsed,
	 * link-rewritten, and validated at build time by `$lib/docs/site` — so this
	 * component only places it and the navigation around it. `{@html}` is safe
	 * for the same reason the manifest is a boundary: the only strings that
	 * reach it come from the 27 sources the manifest names.
	 * What: the page heading comes from the markdown itself (every published
	 * source opens with one), so nothing is restated; the manifest title drives
	 * `<title>` and the nav. The on-this-page rail lists h2/h3 only.
	 */
	let { data } = $props();

	const doc = $derived(data.page);
</script>

<svelte:head>
	<title>{doc.title} — trusty-tools</title>
	<meta name="description" content="{doc.title} — trusty-tools documentation." />
</svelte:head>

<div class="min-w-0 py-10 xl:grid xl:grid-cols-[minmax(0,1fr)_13rem] xl:gap-10">
	<article class="min-w-0">
		<p class="eyebrow">{doc.sectionTitle}</p>
		<div class="doc-prose mt-3">
			<!-- eslint-disable-next-line svelte/no-at-html-tags -- build-time HTML, see above -->
			{@html doc.html}
		</div>

		<footer class="mt-16 border-t border-foundry-border pt-6">
			<a
				href={doc.sourceUrl}
				rel="noreferrer noopener"
				class="font-mono text-xs text-foundry-secondary underline decoration-dotted underline-offset-4 hover:text-foundry-text"
			>
				View source: {doc.source}
			</a>

			<nav aria-label="Pagination" class="mt-6 flex flex-col gap-3 sm:flex-row sm:justify-between">
				{#if doc.prev}
					<a href={doc.prev.href} class="btn btn-secondary sm:max-w-[48%]">
						<span aria-hidden="true">&larr;</span>
						<span class="truncate">{doc.prev.title}</span>
					</a>
				{:else}
					<span></span>
				{/if}
				{#if doc.next}
					<a href={doc.next.href} class="btn btn-secondary sm:ml-auto sm:max-w-[48%]">
						<span class="truncate">{doc.next.title}</span>
						<span aria-hidden="true">&rarr;</span>
					</a>
				{/if}
			</nav>
		</footer>
	</article>

	{#if doc.toc.length > 1}
		<aside
			aria-label="On this page"
			class="sticky top-20 hidden max-h-[calc(100vh-6rem)] self-start overflow-y-auto xl:block"
		>
			<h2 class="font-mono text-xs uppercase tracking-[0.18em] text-foundry-secondary">
				On this page
			</h2>
			<ul class="mt-2 space-y-1 border-l-[1.5px] border-foundry-border">
				{#each doc.toc as entry (entry.id)}
					<li>
						<a
							href="#{entry.id}"
							class="block py-0.5 text-sm text-foundry-secondary transition-colors hover:text-foundry-text
								{entry.depth === 3 ? 'pl-6' : 'pl-3'}"
						>
							{entry.text}
						</a>
					</li>
				{/each}
			</ul>
		</aside>
	{/if}
</div>
