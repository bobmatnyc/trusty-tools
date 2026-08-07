<script lang="ts">
	import {
		CRATE_GROUPS,
		FACTS,
		FLAGSHIPS,
		GITHUB_URL,
		INSTALL_OPTIONS,
		STABLE_SET
	} from '$lib/site';

	const description =
		'trusty-tools is one Cargo workspace for the trusty-* ecosystem: hybrid code search, a memory palace, and a code-analysis sidecar, each with an MCP server.';
</script>

<svelte:head>
	<title>trusty-tools — Rust tooling for AI-assisted development</title>
	<meta name="description" content={description} />
</svelte:head>

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6 sm:py-24">
		<p class="eyebrow">Rust workspace · MIT</p>
		<h1
			class="mt-4 max-w-3xl font-display text-4xl font-bold leading-tight tracking-tight sm:text-5xl md:text-6xl"
		>
			Tooling for AI-assisted development, in
			<span class="text-foundry-primary">one workspace</span>.
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			{description}
		</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="#install" class="btn btn-primary">Install</a>
			<a href={GITHUB_URL} rel="noreferrer noopener" class="btn btn-secondary">View on GitHub</a>
		</div>

		<dl class="mt-12 flex flex-wrap gap-x-10 gap-y-4">
			{#each FACTS as fact (fact.label)}
				<div>
					<dt class="eyebrow">{fact.label}</dt>
					<dd class="mt-1 font-mono text-sm text-foundry-text">{fact.value}</dd>
				</div>
			{/each}
		</dl>
	</div>
</section>

<!-- WHAT IT IS -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<h2 class="font-display text-2xl font-bold sm:text-3xl">What trusty-tools is</h2>
	<div class="mt-6 grid gap-6 md:grid-cols-2">
		<p class="text-foundry-secondary">
			A single Cargo workspace holding the whole trusty-* family — shared libraries, daemons and MCP
			servers, a multi-agent platform, and an orchestrator. It replaces seven formerly separate
			repositories, so cross-crate changes are one atomic commit instead of a chain of version bumps
			and patch tables.
		</p>
		<p class="text-foundry-secondary">
			Each crate versions, tags, and publishes independently, and everything is MIT licensed. The
			three flagship services below each speak the Model Context Protocol, so they plug into Claude
			Code and any other MCP client without a bespoke integration.
		</p>
	</div>
</section>

<!-- FLAGSHIP SERVICES -->
<section class="mx-auto max-w-content px-4 pb-16 sm:px-6">
	<h2 class="font-display text-2xl font-bold sm:text-3xl">Three flagship MCP servers</h2>
	<div class="mt-8 grid gap-6 lg:grid-cols-3">
		{#each FLAGSHIPS as flagship (flagship.name)}
			<article class="card flex flex-col">
				<p class="eyebrow">{flagship.unit}</p>
				<h3 class="mt-2 break-words font-display text-xl font-semibold text-foundry-primary">
					{flagship.name}
				</h3>
				<p class="mt-1 text-sm font-semibold text-foundry-text">{flagship.tagline}</p>
				<ul class="mt-4 space-y-2 text-sm text-foundry-secondary">
					{#each flagship.points as point (point)}
						<li class="flex gap-2">
							<span aria-hidden="true" class="mt-[0.45em] h-1 w-1 shrink-0 bg-foundry-primary"
							></span>
							<span>{point}</span>
						</li>
					{/each}
				</ul>
			</article>
		{/each}
	</div>
</section>

<!-- CRATE LINEUP -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">The crate lineup</h2>
		<p class="mt-3 max-w-2xl text-foundry-secondary">
			A selection of what lives under <code class="text-sm">crates/</code>. The workspace glob in
			the root <code class="text-sm">Cargo.toml</code> is the authoritative list.
		</p>
		<div class="mt-8 grid gap-8 sm:grid-cols-2 lg:grid-cols-4">
			{#each CRATE_GROUPS as group (group.group)}
				<div>
					<h3 class="eyebrow">{group.group}</h3>
					<ul class="mt-3 space-y-3">
						{#each group.crates as crate (crate.name)}
							<li>
								<p class="break-words font-mono text-sm text-foundry-text">{crate.name}</p>
								<p class="mt-0.5 text-sm text-foundry-secondary">{crate.description}</p>
							</li>
						{/each}
					</ul>
				</div>
			{/each}
		</div>
	</div>
</section>

<!-- INSTALL -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<h2 id="install" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">Install</h2>
	<p class="mt-3 max-w-2xl text-foundry-secondary">
		A bare <code class="text-sm">tctl install</code> brings up the seven managed crates in dependency
		order. Name one to install just that crate and whatever it needs at runtime.
	</p>
	<div class="mt-8 grid gap-6 lg:grid-cols-3">
		{#each INSTALL_OPTIONS as option (option.id)}
			<!-- `min-w-0`: a grid item's automatic minimum size is its content's
			     min-content width, and a `<pre>` never wraps. Without this the
			     card widens to the longest command and the whole page scrolls
			     sideways instead of the `<pre>` scrolling inside it. -->
			<div class="card flex min-w-0 flex-col">
				<h3 class="font-display text-lg font-semibold">{option.title}</h3>
				<p class="mt-1 text-sm text-foundry-secondary">{option.note}</p>
				<pre
					class="mt-4 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-raised p-3 text-xs leading-relaxed text-foundry-text">{option.command}</pre>
			</div>
		{/each}
	</div>
	<p class="mt-6 max-w-3xl text-sm text-foundry-secondary">
		<span class="eyebrow">Managed by tctl</span><br />
		<span class="font-mono">{STABLE_SET.join(' · ')}</span>
	</p>
	<p class="mt-6 max-w-3xl text-sm text-foundry-secondary">
		The bootstrap installer verifies every downloaded tarball against its published SHA-256
		checksum. The install script itself is unsigned — read it first if you need higher assurance.
	</p>
</section>

<!-- NEXT -->
<section class="mx-auto max-w-content px-4 pb-8 sm:px-6">
	<div class="card flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
		<div>
			<h2 class="font-display text-xl font-semibold">Read the documentation</h2>
			<p class="mt-1 text-sm text-foundry-secondary">
				Guides, references, and architecture decisions for every crate.
			</p>
		</div>
		<a href="/docs" class="btn btn-primary shrink-0">Browse docs</a>
	</div>
</section>
