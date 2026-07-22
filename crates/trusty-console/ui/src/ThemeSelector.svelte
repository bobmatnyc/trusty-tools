<script>
  /**
   * Why: Provides an accessible, compact way to switch between Light/Dark/System
   *      themes without a dropdown — segmented controls are immediately visible.
   * What: Renders three buttons (Light/Dark/System); clicking sets theme.current
   *       and persists to localStorage via the theme module.
   * Test: Click 'Light' — assert data-theme='light' on <html>; click 'System' —
   *       assert data-theme follows matchMedia result.
   */
  import { theme } from './theme.svelte.js';

  const OPTIONS = [
    { value: 'light', label: 'Light' },
    { value: 'dark',  label: 'Dark' },
    { value: 'system', label: 'System' },
  ];
</script>

<div class="theme-selector" role="group" aria-label="Color theme">
  {#each OPTIONS as opt (opt.value)}
    <button
      class="theme-btn"
      class:active={theme.current === opt.value}
      onclick={() => { theme.current = opt.value; }}
      aria-pressed={theme.current === opt.value}
    >
      {opt.label}
    </button>
  {/each}
</div>

<style>
  .theme-selector {
    display: flex;
    gap: 0;
    border: 1px solid var(--trusty-border);
    border-radius: 0.4rem;
    overflow: hidden;
  }
  .theme-btn {
    background: none;
    border: none;
    border-right: 1px solid var(--trusty-border);
    color: var(--trusty-text-secondary);
    cursor: pointer;
    font-size: 0.72rem;
    font-weight: 500;
    padding: 0.25rem 0.6rem;
    transition: background 0.15s, color 0.15s;
    line-height: 1.4;
  }
  .theme-btn:last-child {
    border-right: none;
  }
  .theme-btn:hover {
    background: var(--trusty-border);
    color: var(--trusty-text-primary);
  }
  .theme-btn.active {
    background: var(--trusty-accent);
    color: #ffffff;
  }
</style>
