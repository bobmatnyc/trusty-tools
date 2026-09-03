<!--
  Foundry StatCard, ported for the machine-status dashboard (#6518).

  Why: the dashboard's top row is the Foundry stat-card grid
  (docs/design/UI/design-system-svelte/src/screens/search/Dashboard.svelte).
  What: markup and props are a verbatim copy of that system's
  src/lib/StatCard.svelte; the .stat/.stat-label/.stat-value/.stat-meta classes
  it references are ported alongside it into foundry.css. `children` renders
  between the value and the meta line, which is where the dashboard puts each
  subsystem's pressure Badge. #6642 adds one deviation from the port: an
  optional `footer` snippet rendered flush to the card's bottom edge, which is
  where the owner's ruling puts the 1 s history bar graph.
  Test: `machineStatus.test.js` covers the values fed to it (`statCards`);
  `barGraph.test.js` covers what the footer draws.
-->
<script>
  let { label, value, meta = '', accent = false, children, footer } = $props();
</script>

<div class="stat" class:has-footer={footer}>
  <div class="stat-label">{label}</div>
  {#if value}<div class="stat-value" class:accent>{value}</div>{/if}
  {@render children?.()}
  {#if meta}<div class="stat-meta">{meta}</div>{/if}
  {#if footer}
    <!-- #6642: the graph sits on the card's bottom edge, so the card's own
         padding is cancelled here rather than the graph being inset. -->
    <div class="stat-footer">{@render footer()}</div>
  {/if}
</div>

<style>
  .accent { color: var(--trusty-accent); }
  /* The graph is flush left-to-right and flush to the bottom; the card keeps
     its padding for everything above it. Three classes so this beats
     `.foundry .stat`'s own padding in foundry.css whichever sheet loads first. */
  .stat.has-footer {
    display: flex;
    flex-direction: column;
    padding-bottom: 0;
    overflow: hidden;
  }
  /* `margin-top: auto` pins the graph to the bottom of a stretched grid
     cell, so the four cards' graphs line up whatever height the tallest
     card's text forces. */
  .stat-footer { margin: auto -20px 0; padding-top: 14px; }
</style>
