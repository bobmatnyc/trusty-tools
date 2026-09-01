<script>
  /*
   * Why: this SPA writes `class="badge badge-success"` by hand at every call
   * site, which is fine until a badge needs a spinner or a dot — then each site
   * grows its own markup and its own `@keyframes spin`, and they drift. Foundry
   * already settled the shape (docs/design/UI/design-system-svelte/src/lib/
   * Badge.svelte); this is that component, ported so the indexing-pipeline row
   * (#6524) has one badge to render instead of three variants of one.
   * What: a thin wrapper over the `.badge` / `.badge-<tone>` classes already in
   * `styles/global.css`, adding the inline-flex alignment and the optional
   * leading spinner or dot Foundry defines. `tone` is '' (accent), 'success',
   * 'warning', 'danger', 'info' or 'muted'.
   * Test: exercised by the pipeline row; the tone mapping that feeds it is
   * covered by `indexingPipeline.test.js`.
   */
  let { tone = '', dot = false, spinner = false, children } = $props();
</script>

<span class="badge inline {tone ? 'badge-' + tone : ''}">
  {#if spinner}<span class="spinner"></span>{:else if dot}<span class="dot"></span>{/if}
  {@render children?.()}
</span>

<style>
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    flex: none;
  }
  .spinner {
    width: 9px;
    height: 9px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    flex: none;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  /* Respect a reduced-motion preference: keep the shape, drop the rotation. */
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
    }
  }
</style>
