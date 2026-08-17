<script lang="ts">
	/**
	 * Why: the flagship pages share a hero, an install block, and a footer of
	 * outbound links. Writing that markup once per page would let the pages
	 * drift apart visually and would repeat the `<svelte:head>` wiring as many
	 * ways. The BODY is not shared — each page writes its own sections through
	 * the `children` snippet, because the copy is the point.
	 *
	 * What: chrome only. Takes the tool's record from `$lib/tools`, plus the
	 * facts strip that page wants above the fold.
	 *
	 * Test: `tests/build-smoke.test.ts` walks every emitted `tools/*.html`
	 * and asserts it loads no third-party subresource.
	 */
	import { GITHUB_URL } from '$lib/site';
	import { installCommand, type Tool } from '$lib/tools';

	interface Props {
		tool: Tool;
		/** Short label/value pairs rendered under the lede. */
		facts: { label: string; value: string }[];
		children: import('svelte').Snippet;
	}

	let { tool, facts, children }: Props = $props();

	const sourceUrl = $derived(`${GITHUB_URL}/tree/main/crates/${tool.name}`);
</script>

<svelte:head>
	<title>{tool.name} — {tool.tagline} — trusty-tools</title>
	<meta name="description" content={tool.lede} />
</svelte:head>

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">{tool.unit} · {tool.tagline}</p>
		<h1
			class="mt-4 break-words font-display text-4xl font-bold leading-tight tracking-tight text-foundry-primary sm:text-5xl"
		>
			{tool.name}
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">{tool.lede}</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="#install" class="btn btn-primary">Install</a>
			{#if tool.docsPath}
				<a href={tool.docsPath} class="btn btn-secondary">Read the docs</a>
			{/if}
			<a href={sourceUrl} rel="noreferrer noopener" class="btn btn-secondary">Source</a>
		</div>

		<dl class="mt-12 flex flex-wrap gap-x-10 gap-y-4">
			{#each facts as fact (fact.label)}
				<div>
					<dt class="eyebrow">{fact.label}</dt>
					<dd class="mt-1 font-mono text-sm text-foundry-text">{fact.value}</dd>
				</div>
			{/each}
		</dl>
	</div>
</section>

<!-- BODY — each page's own copy. -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="flex flex-col gap-12">
		{@render children()}
	</div>
</section>

<!-- INSTALL -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<h2 id="install" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">Install</h2>
		{#if tool.install.via === 'tctl'}
			<p class="mt-3 max-w-2xl text-foundry-secondary">
				<code class="text-sm">tctl</code> resolves whatever else this crate needs at runtime and
				keeps macOS signing grants stable across upgrades. The
				<a href="/#install" class="text-foundry-primary underline underline-offset-2"
					>other install paths</a
				>
				— Homebrew, or <code class="text-sm">cargo install</code> from source — are on the home page.
			</p>
		{:else}
			<p class="mt-3 max-w-2xl text-foundry-secondary">{tool.install.note}</p>
		{/if}
		<!-- `min-w-0`: a `<pre>` never wraps, so without it the flex child
		     widens to the longest command and the page scrolls sideways. -->
		<div class="mt-6 max-w-xl min-w-0">
			<pre
				class="overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">{installCommand(
					tool
				)}</pre>
		</div>
		<p class="mt-6 max-w-2xl text-sm text-foundry-secondary">
			Build and test this crate from a checkout with
			<code class="text-sm">cargo test -p {tool.cargoPackage}</code>.
		</p>
	</div>
</section>

<!-- NEXT -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="card flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
		<div>
			<h2 class="font-display text-xl font-semibold">
				{#if tool.docsPath}Go deeper{:else}Read the source{/if}
			</h2>
			<p class="mt-1 text-sm text-foundry-secondary">
				{#if tool.docsPath}
					Reference documentation for {tool.name}, plus every other crate in the workspace.
				{:else}
					{tool.name} publishes no documentation page yet — the crate's own README and source are the
					reference.
				{/if}
			</p>
		</div>
		<a
			href={tool.docsPath ?? sourceUrl}
			rel={tool.docsPath ? undefined : 'noreferrer noopener'}
			class="btn btn-primary shrink-0"
		>
			{tool.docsPath ? 'Browse docs' : 'View on GitHub'}
		</a>
	</div>
	<p class="mt-8">
		<a href="/#flagships" class="text-sm text-foundry-primary underline underline-offset-2"
			>← All flagship tools</a
		>
	</p>
</section>
