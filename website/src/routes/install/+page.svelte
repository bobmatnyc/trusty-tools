<script lang="ts">
	/**
	 * Why: the landing page's install section answers "how do I install
	 * something" with three generic paths. It does not answer the question a
	 * reader actually arrives with, which is "how do I install THIS one" — and
	 * the nine answers differ in ways that are not guessable: two products
	 * cannot be installed by tctl at all, one of them has no published crate,
	 * one silently installs three crates, and the macOS permission is
	 * per-product. Publishing that permission backwards would be a security
	 * regression rather than a typo, which is why every fact on this page comes
	 * from one verified document (#5110, epic #5092).
	 *
	 * What: chrome and framing only. The content is `$lib/install/audiences`
	 * and the picker is `InstallWalkthrough.svelte`; this file adds the hero,
	 * the `<svelte:head>` wiring and the outbound links. It is not built on
	 * `ToolPage.svelte` for the same reason `/claude-mpm-migration` is not:
	 * that component derives its heading and its single install line from one
	 * `Tool` record, and this page is about nine products at once.
	 *
	 * Test: `src/lib/install/render.test.ts` covers the walkthrough itself;
	 * `tests/build-smoke.test.ts` asserts this route prerenders with every
	 * audience's commands in it.
	 */
	import InstallWalkthrough from '$lib/components/InstallWalkthrough.svelte';
	import { AUDIENCES } from '$lib/install/audiences';
	import { GITHUB_URL } from '$lib/site';

	const description =
		'Install any of the nine trusty-tools products: the exact commands per product, what each needs first, which MCP entry to register, and which macOS permission it actually asks for.';

	const facts = [
		{ label: 'Paths', value: `${AUDIENCES.length} audiences` },
		{ label: 'Via tctl', value: '7 of 9' },
		{ label: 'MSRV', value: 'Rust 1.94' },
		{ label: 'Prebuilt for', value: 'macOS arm64, Linux x86_64, Linux arm64' }
	];
</script>

<svelte:head>
	<title>Install — pick your product — trusty-tools</title>
	<meta name="description" content={description} />
</svelte:head>

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">Install · nine products, nine paths</p>
		<h1
			class="mt-4 break-words font-display text-4xl font-bold leading-tight tracking-tight text-foundry-primary sm:text-5xl"
		>
			Install walkthrough
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			Pick the product you want and this page shows that install and nothing else — the commands in
			order, the setup it shares with the others, the MCP entry to register, and the macOS
			permission it genuinely needs. Every command here was checked against crate source rather than
			against a README.
		</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="#walkthrough" class="btn btn-primary">Pick a product</a>
			<a href="#prerequisites" class="btn btn-secondary">Shared prerequisites</a>
			<a href="/#flagships" class="btn btn-secondary">What each one does</a>
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

<InstallWalkthrough />

<!-- NEXT -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="card flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
		<div class="min-w-0">
			<h2 class="font-display text-xl font-semibold">Already running claude-mpm?</h2>
			<p class="mt-1 text-sm text-foundry-secondary">
				The migration page covers what carries over and what behaves differently, rather than
				repeating the install.
			</p>
		</div>
		<a href="/claude-mpm-migration" class="btn btn-primary shrink-0">Migration guide</a>
	</div>
	<p class="mt-8 flex flex-wrap gap-x-6 gap-y-2">
		<a href="/docs" class="text-sm text-foundry-primary underline underline-offset-2"
			>Documentation</a
		>
		<a
			href={GITHUB_URL}
			rel="noreferrer noopener"
			class="text-sm text-foundry-primary underline underline-offset-2">Source on GitHub</a
		>
	</p>
</section>
