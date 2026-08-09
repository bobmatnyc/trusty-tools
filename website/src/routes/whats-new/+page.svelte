<script lang="ts">
	/**
	 * Why: the crates' `CHANGELOG.md` files are the source of truth for what
	 * shipped; this page only organises them. Everything below is already HTML
	 * by the time it arrives — parsed and serialised at build time by
	 * `$lib/changelog` — so `{@html}` is placing markup this build produced from
	 * six files in this repository, not rendering anything from a request.
	 *
	 * What: one section per flagship in `FLAGSHIPS` order, each with
	 * `id="<crate-name>"` so the landing page's "All changes →" lands on it.
	 * Recent releases carry their items; every older one is listed by version
	 * and date inside a native `<details>`, which keeps 118 trusty-search
	 * releases from burying the next crate and keeps the page off the 2.4 MB it
	 * weighed when it carried all 234 in full (`DETAILED_RELEASES`). The whole
	 * file is one click away in every section header.
	 */
	import { GITHUB_URL } from '$lib/site';

	let { data } = $props();

	const description =
		'What shipped in the six flagship trusty-tools crates, generated from the crates’ own CHANGELOG.md files.';
</script>

<svelte:head>
	<title>What’s new — trusty-tools</title>
	<meta name="description" content={description} />
</svelte:head>

{#snippet release(entry: (typeof data.crates)[number]['detailed'][number])}
	<article class="border-t border-foundry-border pt-6">
		<h3 class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
			<span class="font-mono text-lg font-semibold text-foundry-text">{entry.version}</span>
			{#if entry.date && entry.date !== entry.version}
				<span class="font-mono text-sm text-foundry-secondary">{entry.date}</span>
			{/if}
			{#if entry.title}
				<span class="text-sm text-foundry-secondary">{entry.title}</span>
			{/if}
		</h3>

		{#if entry.preambleHtml}
			<div class="doc-prose mt-4 text-sm">
				<!-- eslint-disable-next-line svelte/no-at-html-tags -- build-time HTML, see above -->
				{@html entry.preambleHtml}
			</div>
		{/if}

		{#each entry.categories as category (category.label)}
			<div class="mt-5">
				<h4 class="eyebrow">{category.label}</h4>
				{#each category.blocks as block, index (index)}
					{#if block.kind === 'items'}
						<ul class="doc-prose mt-2 list-disc space-y-2 pl-5 text-sm">
							{#each block.items as item, itemIndex (itemIndex)}
								<li>
									<!-- eslint-disable-next-line svelte/no-at-html-tags -- build-time HTML, see above -->
									{@html item.html}
								</li>
							{/each}
						</ul>
					{:else}
						<div class="doc-prose mt-2 text-sm">
							<!-- eslint-disable-next-line svelte/no-at-html-tags -- build-time HTML, see above -->
							{@html block.html}
						</div>
					{/if}
				{/each}
			</div>
		{/each}
	</article>
{/snippet}

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">Release history</p>
		<h1 class="mt-4 max-w-3xl font-display text-4xl font-bold tracking-tight sm:text-5xl">
			What’s new
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			{description}
		</p>

		<nav aria-label="Jump to a crate" class="mt-8 flex flex-wrap gap-2">
			{#each data.crates as crate (crate.name)}
				<a href="#{crate.name}" class="badge hover:border-foundry-primary hover:text-foundry-text">
					{crate.name}
					<span class="text-foundry-primary">{crate.detailed[0].version}</span>
				</a>
			{/each}
		</nav>
	</div>
</section>

{#each data.crates as crate (crate.name)}
	<section id={crate.name} class="scroll-mt-24 border-b border-foundry-border">
		<div class="mx-auto max-w-content px-4 py-14 sm:px-6">
			<header>
				<h2 class="break-words font-display text-2xl font-bold sm:text-3xl">
					<a
						href="/tools/{crate.slug}"
						class="text-foundry-primary underline decoration-1 underline-offset-4 hover:decoration-2"
					>
						{crate.name}
					</a>
				</h2>
				<p class="mt-1 text-sm font-semibold text-foundry-text">{crate.tagline}</p>
				<p class="mt-3 text-sm text-foundry-secondary">
					{crate.releaseCount} releases.
					<!-- Unpinned on purpose. The doc reader pins every in-repo link to a
					     commit SHA so cited lines cannot drift; this one points at a
					     LIVING document, and a reader who follows it wants the changelog
					     as it stands now. Do not "fix" this into a blob/<sha> link. -->
					<a
						href={crate.sourceUrl}
						rel="noreferrer noopener"
						class="text-foundry-primary underline decoration-1 underline-offset-2 hover:decoration-2"
					>
						{crate.source}
					</a>
					on GitHub is the source this section is generated from.
				</p>
			</header>

			<div class="mt-8 space-y-8">
				{#each crate.detailed as entry (entry.version + entry.line)}
					{@render release(entry)}
				{/each}
			</div>

			{#if crate.earlier.length > 0}
				<details class="mt-8 border-t border-foundry-border pt-6">
					<summary
						class="cursor-pointer rounded-sm font-mono text-xs uppercase tracking-wider text-foundry-primary"
					>
						{crate.earlier.length} earlier releases
					</summary>
					<!-- Version and date only. The items are in the file linked above;
					     carrying all of them here made the page 2.4 MB. -->
					<ul class="mt-4 space-y-1.5">
						{#each crate.earlier as entry (entry.version)}
							<li class="flex flex-wrap items-baseline gap-x-3 text-sm">
								<span class="font-mono text-foundry-text">{entry.version}</span>
								{#if entry.date && entry.date !== entry.version}
									<span class="font-mono text-xs text-foundry-secondary">{entry.date}</span>
								{/if}
								{#if entry.title}
									<span class="min-w-0 text-foundry-secondary">{entry.title}</span>
								{/if}
							</li>
						{/each}
					</ul>
					<p class="mt-4 text-sm text-foundry-secondary">
						What each of these changed is in
						<a
							href={crate.sourceUrl}
							rel="noreferrer noopener"
							class="text-foundry-primary underline decoration-1 underline-offset-2 hover:decoration-2"
						>
							{crate.source}
						</a>.
					</p>
				</details>
			{/if}
		</div>
	</section>
{/each}

<section class="mx-auto max-w-content px-4 py-14 sm:px-6">
	<div class="card">
		<h2 class="font-display text-xl font-semibold">The rest of the workspace</h2>
		<p class="mt-2 text-sm text-foundry-secondary">
			Only the six flagship crates are published here. Every other crate keeps its changelog
			alongside its source — see
			<a
				href={data.cratesDirUrl}
				rel="noreferrer noopener"
				class="text-foundry-primary underline decoration-1 underline-offset-2 hover:decoration-2"
			>
				crates/
			</a>
			in the repository. Nothing on this page is hand-written: it is generated at build time from the
			same files, and
			<a
				href={GITHUB_URL}
				rel="noreferrer noopener"
				class="text-foundry-primary underline decoration-1 underline-offset-2 hover:decoration-2"
			>
				the repository
			</a>
			remains the source of truth.
		</p>
	</div>
</section>
