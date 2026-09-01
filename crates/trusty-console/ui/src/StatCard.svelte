<!--
  Foundry StatCard, ported for the machine-status dashboard (#6518).

  Why: the dashboard's top row is the Foundry stat-card grid
  (docs/design/UI/design-system-svelte/src/screens/search/Dashboard.svelte).
  What: markup and props are a verbatim copy of that system's
  src/lib/StatCard.svelte; the .stat/.stat-label/.stat-value/.stat-meta classes
  it references are ported alongside it into foundry.css. `children` renders
  between the value and the meta line, which is where the dashboard puts each
  subsystem's pressure Badge.
  Test: `machineStatus.test.js` covers the values fed to it (`statCards`).
-->
<script>
  let { label, value, meta = '', accent = false, children } = $props();
</script>

<div class="stat">
  <div class="stat-label">{label}</div>
  {#if value}<div class="stat-value" class:accent>{value}</div>{/if}
  {@render children?.()}
  {#if meta}<div class="stat-meta">{meta}</div>{/if}
</div>

<style>
  .accent { color: var(--trusty-accent); }
</style>
