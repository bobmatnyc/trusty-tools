<script lang="ts">
	import { page } from '$app/state';
	import ThemeToggle from './ThemeToggle.svelte';
	import { GITHUB_URL, NAV_LINKS } from '$lib/site';

	// `aria-current="page"` is the only nav-state carrier a screen reader gets;
	// the rust underline below is the visual half of the same signal.
	const isCurrent = (href: string) =>
		href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
</script>

<header
	class="sticky top-0 z-20 border-b border-foundry-border bg-foundry-bg/95 backdrop-blur supports-[backdrop-filter]:bg-foundry-bg/80"
>
	<!-- Below `sm` the wordmark, nav, and toggle do not fit one 320px row, so
	     the nav wraps to a second full-width row rather than forcing the page
	     to scroll sideways. Nothing is hidden at any width. -->
	<div
		class="mx-auto flex max-w-content flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3 sm:flex-nowrap sm:px-6"
	>
		<a href="/" class="flex shrink-0 items-center gap-2 rounded-sm">
			<!-- Same four-plate mark as static/favicon.svg, recoloured from tokens. -->
			<svg viewBox="0 0 32 32" aria-hidden="true" class="h-6 w-6 shrink-0">
				<rect x="0" y="0" width="14" height="14" class="fill-foundry-primary" />
				<rect x="18" y="0" width="14" height="14" class="fill-foundry-primary/35" />
				<rect x="0" y="18" width="14" height="14" class="fill-foundry-primary/35" />
				<rect x="18" y="18" width="14" height="14" class="fill-foundry-primary" />
			</svg>
			<span class="font-display text-lg font-bold tracking-tight">trusty-tools</span>
		</a>

		<!-- `flex-wrap` on the nav ITSELF, not just on the header row above it:
		     the row's own wrap only decides whether the nav gets its own line,
		     and once the nav carried a fourth link (#6268) its five items no
		     longer fit that line at 320px — every page read
		     `document.scrollWidth` 323 against a 320 clientWidth. -->
		<nav
			aria-label="Main"
			class="order-last flex w-full flex-wrap items-center gap-1 sm:order-none sm:ml-auto sm:w-auto sm:gap-2"
		>
			{#each NAV_LINKS as link (link.href)}
				<a
					href={link.href}
					aria-current={isCurrent(link.href) ? 'page' : undefined}
					class="rounded-sm px-2 py-1 font-mono text-xs uppercase tracking-wider transition-colors sm:text-sm
						{isCurrent(link.href)
						? 'text-foundry-primary underline decoration-2 underline-offset-[6px]'
						: 'text-foundry-secondary hover:text-foundry-text'}"
				>
					{link.label}
				</a>
			{/each}
			<a
				href={GITHUB_URL}
				rel="noreferrer noopener"
				class="rounded-sm px-2 py-1 font-mono text-xs uppercase tracking-wider text-foundry-secondary transition-colors hover:text-foundry-text sm:text-sm"
			>
				GitHub
			</a>
		</nav>

		<div class="ml-auto shrink-0 sm:ml-0">
			<ThemeToggle />
		</div>
	</div>
</header>
