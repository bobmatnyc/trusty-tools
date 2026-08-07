<script lang="ts">
	import { onMount } from 'svelte';
	import { applyTheme, persistTheme, readStoredTheme, watchSystemTheme } from '$lib/theme';
	import type { ThemeMode } from '$lib/theme';

	/**
	 * Why: every route is prerendered, so the initial HTML cannot know the
	 * reader's stored preference — `app.html`'s pre-paint snippet sets the
	 * class, and this component adopts that same preference on mount so the
	 * control shows the truth rather than a hardcoded default.
	 * What: a three-state segmented control (Light / System / Dark) built as a
	 * WAI-ARIA radiogroup with ROVING TABINDEX: the group is one Tab stop and
	 * arrow keys move between options, which is what `role="radio"` promises a
	 * screen-reader user. Making all three individually tabbable would be the
	 * easier code and the wrong contract.
	 */
	const OPTIONS: { mode: ThemeMode; label: string; symbol: string }[] = [
		{ mode: 'light', label: 'Light theme', symbol: '☀' },
		{ mode: 'system', label: 'Match system theme', symbol: '◐' },
		{ mode: 'dark', label: 'Dark theme', symbol: '☾' }
	];

	let mode = $state<ThemeMode>('system');
	let buttons: HTMLButtonElement[] = $state([]);

	onMount(() => {
		mode = readStoredTheme();
		applyTheme(mode);
		return watchSystemTheme(() => mode);
	});

	function select(next: ThemeMode) {
		mode = next;
		persistTheme(next);
	}

	/** Arrow/Home/End move selection and focus together, per the radiogroup pattern. */
	function onKeydown(event: KeyboardEvent) {
		const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
		let index: number | undefined;
		if (step !== undefined) {
			index = (OPTIONS.findIndex((o) => o.mode === mode) + step + OPTIONS.length) % OPTIONS.length;
		} else if (event.key === 'Home') {
			index = 0;
		} else if (event.key === 'End') {
			index = OPTIONS.length - 1;
		}
		if (index === undefined) return;
		event.preventDefault();
		select(OPTIONS[index].mode);
		buttons[index]?.focus();
	}
</script>

<div
	role="radiogroup"
	aria-label="Color theme"
	onkeydown={onKeydown}
	class="inline-flex rounded border-[1.5px] border-foundry-border bg-foundry-card p-0.5"
>
	{#each OPTIONS as option, i (option.mode)}
		<button
			bind:this={buttons[i]}
			type="button"
			role="radio"
			aria-checked={mode === option.mode}
			aria-label={option.label}
			title={option.label}
			tabindex={mode === option.mode ? 0 : -1}
			onclick={() => select(option.mode)}
			class="rounded-sm px-2.5 py-1 font-mono text-sm leading-none transition-colors
				{mode === option.mode
				? 'bg-foundry-primary text-foundry-inverse'
				: 'text-foundry-secondary hover:bg-foundry-raised'}"
		>
			<span aria-hidden="true">{option.symbol}</span>
		</button>
	{/each}
</div>
