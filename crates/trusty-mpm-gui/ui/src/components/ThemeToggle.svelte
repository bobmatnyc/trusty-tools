<script lang="ts">
  // Why: Lets the user flip dark/light without a settings page; the theme
  // store owns persistence so this component is purely presentational.
  // What: An icon button showing a Moon in dark mode and a Sun in light mode;
  // clicking calls `toggleTheme()`.
  // Test: Click the button and assert the icon swaps and `<html>` toggles its
  // `data-theme="dark"` attribute.
  import { Moon, Sun } from 'lucide-svelte';
  import { theme, toggleTheme } from '../stores/theme';
</script>

<button
  type="button"
  on:click={toggleTheme}
  aria-label="Toggle theme"
  title="Toggle theme"
  class="rounded-md p-1.5 text-trusty-text hover:bg-trusty-border"
>
  {#if $theme === 'dark'}
    <Moon size={16} />
  {:else}
    <Sun size={16} />
  {/if}
</button>
