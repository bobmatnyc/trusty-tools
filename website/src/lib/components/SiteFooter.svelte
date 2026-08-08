<script lang="ts">
	import { GITHUB_URL, NAV_LINKS } from '$lib/site';

	// The oxide chassis stays dark in BOTH themes (Foundry README). Its text
	// tokens are checked against the chassis background, not the page ground:
	// #E6D8C8 on #2B1C12 is 11.76:1, #A58A6B on #2B1C12 is 5.05:1.
	const year = new Date().getFullYear();
</script>

<footer class="mt-24 bg-foundry-chassis text-foundry-chassis-text">
	<div
		class="mx-auto flex max-w-content flex-col gap-6 px-4 py-10 sm:px-6 md:flex-row md:items-start md:justify-between"
	>
		<div class="max-w-sm">
			<p class="font-display text-base font-semibold">trusty-tools</p>
			<p class="mt-2 text-sm text-foundry-chassis-muted">
				One Cargo workspace for the trusty-* tooling ecosystem. MIT licensed.
			</p>
		</div>

		<nav aria-label="Footer" class="flex flex-col gap-2">
			{#each NAV_LINKS as link (link.href)}
				<a
					href={link.href}
					class="rounded-sm font-mono text-xs uppercase tracking-wider text-foundry-chassis-muted transition-colors hover:text-foundry-chassis-accent"
				>
					{link.label}
				</a>
			{/each}
			<a
				href={GITHUB_URL}
				rel="noreferrer noopener"
				class="rounded-sm font-mono text-xs uppercase tracking-wider text-foundry-chassis-muted transition-colors hover:text-foundry-chassis-accent"
			>
				GitHub
			</a>
		</nav>
	</div>

	<div class="border-t border-foundry-chassis-border">
		<p class="mx-auto max-w-content px-4 py-4 font-mono text-xs text-foundry-chassis-muted sm:px-6">
			© {year} · MIT · github.com/bobmatnyc/trusty-tools
		</p>
	</div>
</footer>
