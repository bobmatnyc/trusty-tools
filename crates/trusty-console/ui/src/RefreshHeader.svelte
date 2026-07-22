<script>
  /**
   * Why: Eliminates the copy-pasted section-header / refresh-btn markup and CSS
   *      that was duplicated across AnalyzeTab, MemoryTab, and SearchTab.
   * What: Renders a flex row with a section title on the left and a Refresh
   *       button on the right; delegates click to the caller via {onRefresh}.
   * Test: Mount with title="Foo", refreshing=false — verify heading text and
   *       enabled button; mount with refreshing=true — verify button is disabled
   *       and shows "Refreshing…".
   */

  /** @type {{ title: string, onRefresh: () => void, refreshing: boolean }} */
  let { title, onRefresh, refreshing } = $props();
</script>

<div class="section-header">
  <h2 class="section-title">{title}</h2>
  <button class="refresh-btn" onclick={onRefresh} disabled={refreshing}>
    {refreshing ? 'Refreshing…' : 'Refresh'}
  </button>
</div>

<style>
  .section-header {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1rem;
  }
  .section-title {
    font-size: 1.25rem; font-weight: 600; margin: 0; color: var(--trusty-text-primary); flex: 1;
  }
  .refresh-btn {
    background: none; border: 1px solid var(--trusty-border-strong); border-radius: 0.4rem;
    color: var(--trusty-accent); cursor: pointer; font-size: 0.78rem; font-weight: 500;
    padding: 0.25rem 0.65rem; transition: background 0.15s, border-color 0.15s;
  }
  .refresh-btn:hover:not(:disabled) { background: rgba(0,0,0,0.06); background: color-mix(in srgb, var(--trusty-accent) 9%, transparent); border-color: var(--trusty-accent); }
  .refresh-btn:disabled { opacity: 0.5; cursor: default; }
</style>
