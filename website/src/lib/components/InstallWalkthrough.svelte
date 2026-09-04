<script lang="ts">
	/**
	 * Why: nine products, nine install sequences, and a large shared middle.
	 * Printing all nine at once buries the four lines a given reader needs;
	 * printing the shared setup inside each one repeats the tctl bootstrap and
	 * the API-key section nine times, and a repeated instruction is one that
	 * gets edited in eight places out of nine.
	 *
	 * What: the shared prerequisites once, then an ARIA tablist over the nine
	 * audiences with one panel each. Every panel is rendered into the
	 * prerendered HTML and hidden with the `hidden` attribute rather than
	 * mounted on selection, so the page ships every command as static HTML and
	 * the picker only changes which one is on screen.
	 *
	 * Keyboard: the tablist follows the ARIA authoring-practices pattern —
	 * roving tabindex, so Tab enters and leaves the whole picker as one stop,
	 * with Left/Right moving between audiences and Home/End jumping to the
	 * ends. Selection follows focus, which is the pattern's default for a
	 * picker whose panels are already in the document.
	 *
	 * Test: `src/lib/install/render.test.ts` mounts this component and asserts
	 * every audience gets a tab and a panel, that each panel carries its own
	 * audience's commands, and that arrow keys move the selection.
	 */
	import AudienceSteps from './AudienceSteps.svelte';
	import CommandBlock from './CommandBlock.svelte';
	import { AUDIENCES, usedPrerequisites } from '$lib/install/audiences';

	const prerequisites = usedPrerequisites();

	let selected = $state(0);
	let tabs: HTMLButtonElement[] = $state([]);

	function select(index: number) {
		selected = (index + AUDIENCES.length) % AUDIENCES.length;
		tabs[selected]?.focus();
	}

	function onTabKeydown(event: KeyboardEvent) {
		const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
		if (step !== 0) {
			event.preventDefault();
			select(selected + step);
		} else if (event.key === 'Home') {
			event.preventDefault();
			select(0);
		} else if (event.key === 'End') {
			event.preventDefault();
			select(AUDIENCES.length - 1);
		}
	}
</script>

<!-- SHARED PREREQUISITES — written once, linked to from every audience. -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<h2 id="prerequisites" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
		Shared prerequisites
	</h2>
	<p class="mt-4 max-w-3xl text-foundry-secondary">
		Set up only what your product's roster names. Nothing here is needed by all nine, and every
		audience below says which of these it wants and how badly.
	</p>
	<div class="mt-8 grid gap-4 sm:grid-cols-2">
		{#each prerequisites as prereq (prereq.id)}
			<div id="prereq-{prereq.id}" class="card min-w-0 scroll-mt-24">
				<h3 class="font-display text-lg font-semibold">{prereq.title}</h3>
				<p class="mt-2 text-sm text-foundry-secondary">{prereq.body}</p>
				{#each prereq.commands as block (block.command)}
					<CommandBlock {block} />
				{/each}
			</div>
		{/each}
	</div>
</section>

<!-- PICKER + PANELS -->
<section class="border-t border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<h2 id="walkthrough" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
			Pick what you are installing
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Nine paths. Seven go through tctl; trusty-code and trusty-agents do not, and are the two
			places the commands genuinely differ rather than just naming a different crate.
		</p>

		<div
			role="tablist"
			aria-label="Install audience"
			aria-orientation="horizontal"
			class="mt-8 flex flex-wrap gap-2"
		>
			{#each AUDIENCES as audience, i (audience.id)}
				<button
					bind:this={tabs[i]}
					type="button"
					role="tab"
					id="tab-{audience.id}"
					aria-selected={i === selected}
					aria-controls="panel-{audience.id}"
					tabindex={i === selected ? 0 : -1}
					onclick={() => (selected = i)}
					onkeydown={onTabKeydown}
					class="rounded-sm border-[1.5px] px-3 py-1.5 font-mono text-xs tracking-wide transition-colors
						{i === selected
						? 'border-foundry-primary bg-foundry-primary text-foundry-inverse'
						: 'border-foundry-border-strong bg-foundry-card text-foundry-secondary hover:border-foundry-primary hover:text-foundry-text'}"
				>
					{audience.label}
				</button>
			{/each}
		</div>

		{#each AUDIENCES as audience, i (audience.id)}
			<div
				id="panel-{audience.id}"
				role="tabpanel"
				aria-labelledby="tab-{audience.id}"
				tabindex="0"
				hidden={i !== selected}
				class="mt-10"
			>
				<AudienceSteps {audience} />
			</div>
		{/each}
	</div>
</section>
