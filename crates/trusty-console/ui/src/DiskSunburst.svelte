<!--
  The Disk view's segmented radial, hand-rolled in SVG (#6929, DOC-73 §16.3).

  Why no charting library: `BarGraph.svelte` established the rule — inline SVG
  only, so the console's charts cannot drift apart — and `d3` belongs to a
  different bundle. Geometry lives in `diskSunburst.js` for the same reason it
  does there: `node --test` can assert a path string and cannot mount this file.
  What: two rings over a labelled centre. Ring 1 is one neutral arc per project;
  ring 2 is one arc per worktree, coloured by staleness tier, with the arcs too
  thin to hover folded into a grey "other" wedge. Every segment is a <button>,
  so the same arc answers a click, a hover and a Tab — a detail panel reachable
  only by pointer would be unreachable on a keyboard.
  Test: `diskSunburst.test.js` covers the tier mapping, the fold and the
  geometry this file only draws.
-->
<script>
  import { formatBytes, tierLabel } from './diskSunburst.js';

  /**
   * @type {{
   *   rings: object,
   *   selectedId?: string|null,
   *   onSelect?: (segment: object|null) => void,
   * }}
   */
  let { rings, selectedId = null, onSelect = () => {} } = $props();

  /** What a segment's `<title>` and accessible name say. */
  function describe(segment) {
    const size = formatBytes(segment.bytes);
    if (segment.kind === 'project') return `${segment.name} — ${size}`;
    if (segment.kind === 'other') return segment.name;
    return `${segment.name} — ${tierLabel(segment.tier)}, ${size}`;
  }
</script>

<svg
  class="sunburst"
  viewBox="0 0 {rings.size} {rings.size}"
  role="group"
  aria-label="Projects and worktrees by size, coloured by staleness tier"
>
  <!-- Centre: the workspace-root total. Not a button — there is nothing below
       the root to open. -->
  <circle class="centre" cx={rings.cx} cy={rings.cy} r={rings.centre.radius} />
  <text class="centre-bytes" x={rings.cx} y={rings.cy - 2} text-anchor="middle">
    {formatBytes(rings.centre.bytes)}
  </text>
  <text class="centre-label" x={rings.cx} y={rings.cy + 14} text-anchor="middle">
    workspace
  </text>

  {#each rings.projects as segment (segment.id)}
    <path
      class="arc project"
      class:selected={selectedId === segment.id}
      d={segment.d}
      fill={segment.color}
      role="button"
      tabindex="0"
      aria-label={describe(segment)}
      onclick={() => onSelect(segment)}
      onmouseenter={() => onSelect(segment)}
      onfocus={() => onSelect(segment)}
      onkeydown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect(segment);
        }
      }}
    >
      <title>{describe(segment)}</title>
    </path>
  {/each}

  {#each rings.worktrees as segment (segment.id)}
    <path
      class="arc worktree"
      class:selected={selectedId === segment.id}
      d={segment.d}
      fill={segment.color}
      role="button"
      tabindex="0"
      aria-label={describe(segment)}
      onclick={() => onSelect(segment)}
      onmouseenter={() => onSelect(segment)}
      onfocus={() => onSelect(segment)}
      onkeydown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect(segment);
        }
      }}
    >
      <title>{describe(segment)}</title>
    </path>
  {/each}
</svg>

<style>
  .sunburst {
    display: block;
    width: 100%;
    max-width: 440px;
    height: auto;
    margin: 0 auto;
  }
  .centre {
    fill: var(--trusty-surface-raised);
    stroke: var(--trusty-border);
  }
  .centre-bytes {
    fill: var(--trusty-text-primary);
    font-size: 15px;
    font-weight: 600;
    font-family: var(--trusty-mono, monospace);
  }
  .centre-label {
    fill: var(--trusty-text-secondary);
    font-size: 9px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }
  /* A hairline between segments, in the page ground rather than a colour of its
     own — the arcs already carry the only meaningful colour on the chart. */
  .arc {
    stroke: var(--trusty-content-bg);
    stroke-width: 1.2;
    cursor: pointer;
    transition: opacity 0.12s;
  }
  .arc:hover,
  .arc:focus-visible {
    opacity: 0.82;
  }
  .arc:focus-visible {
    outline: 2px solid var(--trusty-accent);
    outline-offset: 1px;
  }
  .arc.selected {
    stroke: var(--trusty-text-primary);
    stroke-width: 2;
  }
  .arc.project {
    stroke: var(--trusty-border-strong);
  }
</style>
