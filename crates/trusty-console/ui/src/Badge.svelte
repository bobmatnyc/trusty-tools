<!--
  Foundry Badge, ported for the machine-status dashboard (#6518).

  Why: the dashboard stamps host pressure and per-service health as Foundry
  badges (rectangular stamps, not the pills the other tabs use).
  What: markup and props are a verbatim copy of
  docs/design/UI/design-system-svelte/src/lib/Badge.svelte; the .badge and
  .badge-<tone> classes it composes are ported alongside it into foundry.css,
  scoped under `.foundry`. `ServicesList`'s own hand-rolled status stamp is
  untouched: its colour comes per-status from `statusPresentation.js`, which no
  Foundry tone class can express.
  Test: `machineStatus.test.js` covers the tone each value maps to
  (`pressureTone`). #6643 deleted `serviceHealthTone` and `rollupTone` with the
  last rollup table that stamped them.
-->
<script>
  // tone: '' | 'success' | 'warning' | 'danger' | 'info' | 'muted'
  let { tone = '', dot = false, spinner = false, children } = $props();
</script>

<span class="badge inline {tone ? 'badge-' + tone : ''}">
  {#if spinner}<span class="spinner"></span>{:else if dot}<span class="dot"></span>{/if}
  {@render children?.()}
</span>

<style>
  .inline { display: inline-flex; align-items: center; gap: 6px; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; flex: none; }
</style>
