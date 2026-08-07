<script lang="ts">
	import { page } from '$app/state';

	/**
	 * Why: the sidebar is navigation for the whole `/docs` subtree, so it lives
	 * in a layout and stays mounted across page transitions — scroll position and
	 * the expanded section survive a click. Its order is the manifest's file
	 * order, which is the only ordering the manifest defines.
	 * What: a two-column shell. Below `lg` the sidebar collapses into a native
	 * `<details>` disclosure rather than a scripted drawer, so it works before
	 * hydration and needs no focus trap.
	 */
	let { data, children } = $props();

	const isCurrent = (href: string) => page.url.pathname === href;
</script>

<div class="mx-auto w-full max-w-[86rem] px-4 sm:px-6">
	<div class="lg:grid lg:grid-cols-[15rem_minmax(0,1fr)] lg:gap-10">
		<!-- Mobile: one disclosure, closed by default so the page starts at content. -->
		<details class="mt-6 rounded border-[1.5px] border-foundry-border bg-foundry-card lg:hidden">
			<summary
				class="cursor-pointer rounded px-4 py-3 font-mono text-xs uppercase tracking-wider text-foundry-secondary"
			>
				Documentation menu
			</summary>
			<nav aria-label="Documentation" class="border-t border-foundry-border px-4 py-3">
				{@render sectionList()}
			</nav>
		</details>

		<!-- `lg` and up: sticky rail. `top-20` clears the sticky site header. -->
		<nav
			aria-label="Documentation"
			class="sticky top-20 hidden max-h-[calc(100vh-6rem)] self-start overflow-y-auto py-10 pr-2 lg:block"
		>
			{@render sectionList()}
		</nav>

		{@render children()}
	</div>
</div>

{#snippet sectionList()}
	<ul class="space-y-6">
		{#each data.nav as section (section.id)}
			<li>
				<!-- text-secondary, not text-muted: the muted token is 3.87:1 on the
				     light ground and fails AA at this size (website/README.md). -->
				<h2 class="font-mono text-xs uppercase tracking-[0.18em] text-foundry-secondary">
					{section.title}
				</h2>
				<ul class="mt-2 space-y-px border-l-[1.5px] border-foundry-border">
					{#each section.pages as item (item.href)}
						<li>
							<a
								href={item.href}
								aria-current={isCurrent(item.href) ? 'page' : undefined}
								class="-ml-[1.5px] block border-l-[1.5px] py-1 pl-3 text-sm transition-colors
									{isCurrent(item.href)
									? 'border-foundry-primary font-semibold text-foundry-primary'
									: 'border-transparent text-foundry-secondary hover:border-foundry-border-strong hover:text-foundry-text'}"
							>
								{item.title}
							</a>
						</li>
					{/each}
				</ul>
			</li>
		{/each}
	</ul>
{/snippet}
