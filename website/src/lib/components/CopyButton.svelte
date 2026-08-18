<script lang="ts">
	/**
	 * Why: the install step's command is meant to land in a terminal exactly as
	 * shown. Selecting it by hand risks catching a stray character or missing
	 * the trailing `sh`, and a mouse-only "select the `<pre>`" affordance
	 * leaves keyboard and screen-reader users with no equivalent shortcut.
	 *
	 * What: copies `text` verbatim via `navigator.clipboard.writeText`, swaps
	 * the icon to a checkmark and announces the result through an `aria-live`
	 * region, then reverts after two seconds. `navigator.clipboard` is
	 * undefined on insecure origins and in some browsers, and a write can be
	 * rejected (permission denial); both land in `error`, never a false
	 * `copied` — the source `<pre>` is never touched, so its text stays
	 * selectable either way.
	 *
	 * Test: manually verified against the production build with Chromium
	 * (button renders, is keyboard-focusable, exposes the passed `label` as
	 * its accessible name, and a click populates the clipboard with `text`
	 * unchanged) — see the trusty-audit copy-button PR description. No
	 * component-test harness (`@testing-library/svelte` or similar) is
	 * installed in this package, and adding one is out of scope for this
	 * change.
	 */
	interface Props {
		/** Exact text to copy — no leading prompt, no trailing newline. */
		text: string;
		/** Accessible name for the button, e.g. "Copy install command". */
		label: string;
	}

	let { text, label }: Props = $props();

	type Status = 'idle' | 'copied' | 'error';
	let status = $state<Status>('idle');
	let resetTimer: ReturnType<typeof setTimeout> | undefined;

	function scheduleReset() {
		clearTimeout(resetTimer);
		resetTimer = setTimeout(() => {
			status = 'idle';
		}, 2000);
	}

	async function copy() {
		if (!navigator.clipboard) {
			status = 'error';
			scheduleReset();
			return;
		}
		try {
			await navigator.clipboard.writeText(text);
			status = 'copied';
		} catch {
			status = 'error';
		}
		scheduleReset();
	}
</script>

<button
	type="button"
	class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-sm border border-foundry-border bg-foundry-card text-foundry-secondary transition-colors hover:border-foundry-border-strong hover:text-foundry-text"
	aria-label={label}
	onclick={copy}
>
	{#if status === 'copied'}
		<svg
			viewBox="0 0 20 20"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			class="h-4 w-4 text-foundry-success"
			aria-hidden="true"
		>
			<path d="M4 10.5l3.5 3.5L16 6" stroke-linecap="round" stroke-linejoin="round" />
		</svg>
	{:else}
		<svg
			viewBox="0 0 20 20"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			class="h-4 w-4"
			aria-hidden="true"
		>
			<rect x="6.5" y="6.5" width="9" height="9" rx="1.5" />
			<path d="M4.5 12V5A1.5 1.5 0 0 1 6 3.5h7" stroke-linecap="round" />
		</svg>
	{/if}
</button>
<span class="sr-only" role="status" aria-live="polite">
	{#if status === 'copied'}
		Copied to clipboard
	{:else if status === 'error'}
		Could not copy automatically — select the text and copy it manually
	{/if}
</span>
