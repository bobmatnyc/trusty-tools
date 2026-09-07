<script lang="ts">
	/**
	 * Why: the shell the owner ruling (2026-09-07) asked for — layout plus one
	 * markdown include, so changing what a flagship page SAYS never means
	 * editing Svelte. Everything visible below the hero comes from
	 * `website/src/content/tools/<slug>.md`.
	 * What: `{@html}` over build-time HTML, safe for the same reason the doc
	 * reader's is — the only strings that reach it were rendered by
	 * `$lib/flagship/content` from files in this repository, with every link
	 * resolved and every unknown element failed at build time.
	 */
	import ToolPage from '$lib/components/ToolPage.svelte';

	let { data } = $props();
</script>

<ToolPage tool={data.tool}>
	<!-- eslint-disable-next-line svelte/no-at-html-tags -- build-time HTML, see above -->
	<div class="doc-prose">{@html data.html}</div>
</ToolPage>
