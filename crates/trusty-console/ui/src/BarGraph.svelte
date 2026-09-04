<!--
  The one bar graph the home page draws, everywhere it draws one (#6642).

  Why: the owner's ruling puts a history graph on the bottom edge of every
  metric card and on every service row. One component means the four host cards
  and the six service rows cannot drift apart in bar width, colour band or
  right-to-left direction — and there is exactly one place to change any of
  them.
  What: inline SVG, no charting library, no canvas, no JS timer. `barGraph.js`
  turns the sample array into three path strings — one per colour band — so a
  600-sample redraw rewrites three attributes rather than 600 elements. The
  viewBox is `slots × 100` with preserveAspectRatio="none", so the graph fills
  whatever width its container has and the caller never computes a pixel.
  Newest sample is the rightmost bar, because the array is oldest-first.

  Accessibility: the graph is decorative next to a number that says the same
  thing, so it carries `aria-hidden` and the `label` prop is a `<title>` for a
  pointer hover only. Nothing here animates, so there is no reduced-motion case
  to gate.
  Test: `barGraph.test.js` covers the geometry, the bands, the null gap and the
  600-bar case.
-->
<script>
  import { barPaths } from './barGraph.js';

  /**
   * @type {{
   *   values?: (number|null)[],
   *   max?: number,
   *   thresholds?: { warning: number, critical: number } | null,
   *   label?: string,
   *   height?: string,
   * }}
   */
  let {
    values = [],
    max = 100,
    thresholds = null,
    label = '',
    height = '2rem',
  } = $props();

  let geometry = $derived(barPaths(values, { max, thresholds }));
</script>

<div class="bar-graph" style="--bar-graph-height: {height};">
  <svg
    viewBox="0 0 {geometry.slots} {geometry.height}"
    preserveAspectRatio="none"
    aria-hidden="true"
    focusable="false"
  >
    {#if label}<title>{label}</title>{/if}
    {#if geometry.paths.nominal}
      <path class="band nominal" d={geometry.paths.nominal} />
    {/if}
    {#if geometry.paths.warning}
      <path class="band warning" d={geometry.paths.warning} />
    {/if}
    {#if geometry.paths.critical}
      <path class="band critical" d={geometry.paths.critical} />
    {/if}
  </svg>
</div>

<style>
  .bar-graph {
    display: block;
    width: 100%;
    height: var(--bar-graph-height);
    /* A faint plot area, so a window with no samples yet still reads as an
       empty graph rather than as a missing element. A border would double up
       against the card edge the graph is flush with. */
    background: rgba(0, 0, 0, 0.04);
    background: color-mix(in srgb, var(--trusty-text-muted) 8%, transparent);
  }
  svg {
    display: block;
    width: 100%;
    height: 100%;
  }
  /* Foundry has no chart palette, so the bands reuse the status tokens the
     badges on the same card already use — a bar and its badge cannot disagree
     about what 96% CPU means. */
  .nominal { fill: var(--trusty-accent); }
  .warning { fill: var(--trusty-warning); }
  .critical { fill: var(--trusty-danger); }
  .band { opacity: 0.85; }
</style>
