<script lang="ts">
	/**
	 * Why: one audience's ordered install, rendered on its own. Splitting it
	 * out of the walkthrough is what lets a test mount a single audience and
	 * assert that every command belonging to it — and nothing belonging to
	 * another — reached the DOM.
	 *
	 * What: the prerequisite roster (names and requirement only; the setup
	 * itself is written once, above the picker, and linked back to), the
	 * numbered steps, and the macOS permission note. The permission note reads
	 * its label from the typed `TccCategory` rather than from prose, so the
	 * category a product gets cannot drift from the one the data declares.
	 *
	 * Test: `src/lib/install/render.test.ts`.
	 */
	import CommandBlock from './CommandBlock.svelte';
	import { prerequisite, type Audience } from '$lib/install/audiences';

	let { audience }: { audience: Audience } = $props();

	/** The heading shown above the permission note, per category. */
	const TCC_LABEL = {
		none: 'macOS: no permission needed',
		'full-disk-access': 'macOS: Full Disk Access',
		'app-data': 'macOS: App Data'
	} as const;
</script>

<div class="min-w-0">
	<p class="eyebrow">{audience.tagline} · {audience.binary}</p>
	<p class="mt-3 max-w-3xl text-foundry-secondary">{audience.lede}</p>

	<h3 class="mt-8 font-display text-lg font-semibold">Before you start</h3>
	<ul class="mt-3 flex flex-col gap-2">
		{#each audience.prerequisites as ref (ref.id)}
			<li class="max-w-3xl text-sm text-foundry-secondary">
				<a
					href="#prereq-{ref.id}"
					class="font-semibold text-foundry-primary underline underline-offset-2"
					>{prerequisite(ref.id).title}</a
				>
				<span class="badge ml-2">{ref.requirement}</span>
				<span class="mt-1 block">{ref.note}</span>
			</li>
		{/each}
	</ul>

	<ol class="mt-8 flex flex-col gap-8">
		{#each audience.steps as step, i (step.title)}
			<li class="min-w-0">
				<h3 class="font-display text-lg font-semibold">{i + 1} · {step.title}</h3>
				<p class="mt-2 max-w-3xl text-foundry-secondary">{step.body}</p>
				{#each step.commands as block (block.command)}
					<CommandBlock {block} />
				{/each}
				{#each step.notes as note (note)}
					<p class="mt-3 max-w-3xl text-sm text-foundry-secondary">{note}</p>
				{/each}
			</li>
		{/each}
	</ol>

	<div class="card mt-8 max-w-3xl min-w-0 border-l-4 border-l-foundry-primary">
		<p class="eyebrow">{TCC_LABEL[audience.tcc.category]}</p>
		<p class="mt-3 text-sm text-foundry-secondary">{audience.tcc.summary}</p>
	</div>
</div>
