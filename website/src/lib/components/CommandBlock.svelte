<script lang="ts">
	/**
	 * Why: the install walkthrough prints twenty-odd command blocks and every
	 * one of them owes the same three things — a `<pre>` that scrolls instead
	 * of widening the page, a copy button with its own accessible name, and,
	 * when the command carries a `<placeholder>`, the line that says what to
	 * substitute. Repeating that markup per block is how one of the three goes
	 * missing.
	 *
	 * What: the `<pre>` + `CopyButton` pair the tga audit page established,
	 * lifted into one component, plus the placeholder instruction. The
	 * `min-w-0` and `pr-14` are that page's own containment fix: a `<pre>`
	 * never wraps, so without them the flex child widens to the longest
	 * command and the page scrolls sideways at 375px.
	 *
	 * Test: `src/lib/install/render.test.ts` asserts every command in
	 * `$lib/install/audiences` reaches the DOM with a labelled copy button, and
	 * that no placeholder is rendered without its instruction.
	 */
	import CopyButton from './CopyButton.svelte';
	import { hasPlaceholder, type CommandBlock } from '$lib/install/audiences';

	let { block }: { block: CommandBlock } = $props();
</script>

<div class="mt-4 max-w-3xl min-w-0">
	<div class="relative">
		<pre
			class="overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-4 pr-14 text-xs leading-relaxed text-foundry-text">{block.command}</pre>
		<div class="absolute right-2 top-2">
			<CopyButton text={block.command} label={block.label} />
		</div>
	</div>
	{#if hasPlaceholder(block.command)}
		<p class="mt-2 text-sm text-foundry-secondary">{block.placeholderNote}</p>
	{/if}
</div>
