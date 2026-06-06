<script lang="ts">
  /**
   * Why: Gives users an explicit, discoverable control to switch between
   * light, system, and dark themes — system-only would lock users out of
   * choosing a different palette than their OS preference. A three-way pill
   * toggle is more discoverable than a single icon button that cycles modes.
   * What: Renders three buttons (Light / System / Dark) with inline SVG
   * icons. Highlights the active mode and calls setTheme() on click.
   * Test: Click each button — page palette flips and selection persists
   * across reloads.
   */
  import { theme, setTheme, type Theme } from '../stores/theme';

  const options: { value: Theme; label: string }[] = [
    { value: 'light', label: 'Light' },
    { value: 'system', label: 'System' },
    { value: 'dark', label: 'Dark' },
  ];
</script>

<div
  class="flex items-center gap-1 rounded-lg bg-ompm-light-border dark:bg-ompm-border p-1"
  role="group"
  aria-label="Theme"
>
  {#each options as opt}
    <button
      type="button"
      class="flex items-center gap-1 px-2 py-1 rounded text-xs transition-colors
             {$theme === opt.value
               ? 'bg-ompm-primary text-white'
               : 'text-ompm-light-muted dark:text-ompm-muted hover:text-ompm-light-text dark:hover:text-ompm-text'}"
      on:click={() => setTheme(opt.value)}
      title={opt.label}
      aria-pressed={$theme === opt.value}
    >
      {#if opt.value === 'light'}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="5" />
          <line x1="12" y1="1" x2="12" y2="3" />
          <line x1="12" y1="21" x2="12" y2="23" />
          <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
          <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
          <line x1="1" y1="12" x2="3" y2="12" />
          <line x1="21" y1="12" x2="23" y2="12" />
          <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
          <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
        </svg>
      {:else if opt.value === 'system'}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <rect x="2" y="3" width="20" height="14" rx="2" />
          <line x1="8" y1="21" x2="16" y2="21" />
          <line x1="12" y1="17" x2="12" y2="21" />
        </svg>
      {:else}
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
        </svg>
      {/if}
      <span class="hidden sm:inline">{opt.label}</span>
    </button>
  {/each}
</div>
