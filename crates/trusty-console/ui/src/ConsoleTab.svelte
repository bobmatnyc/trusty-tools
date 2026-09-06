<!--
  The console's own details pane (#6908).

  Why: the Services roster gained a `trusty-console` row, and every other row
  opens a view for the service it names. Without this one the console's row
  would be the single inert entry on a list whose whole affordance is that rows
  open something — and the process holding the SSE fan-out and the sampling loop
  is the one whose cost nothing else on the page reports.
  What: the Foundry dashboard layout the machine-status panel already uses —
  a heading with a status badge, then one `stat-grid four` whose last two cards
  carry the bottom-edge bar graphs. No new layout primitive: `StatCard`,
  `BarGraph` and `Badge` are the same components the Overview draws, and the CPU
  and memory graphs come from `servicesList.js`'s row specs, so this pane and
  the row it opened from draw the same second at the same scale.

  Bus status, service connections and message rates are one sentence rather than
  three empty panels — see `DEFERRED_LINE` in `consoleDetails.js` and the owner
  ruling recorded on #6908.
  Test: `consoleDetails.test.js` covers every value rendered here and asserts
  this file carries the deferred line; the pane itself is verified by the binary
  smoke run against a live daemon.
-->
<script>
  import StatCard from './StatCard.svelte';
  import BarGraph from './BarGraph.svelte';
  import Badge from './Badge.svelte';
  import { rowCpuGraphSpec, rowMemoryGraphSpec, serviceRows } from './servicesList.js';
  import {
    CONSOLE_SERVICE_ID,
    DEFERRED_LINE,
    consoleDetailCards,
    consoleHeading,
    consoleRow,
  } from './consoleDetails.js';

  /**
   * @type {{
   *   services?: object[],
   *   serviceSamples?: Record<string, object[]>,
   *   sseClientCount?: number | null,
   *   version?: string | null,
   *   uptimeSecs?: number | null,
   * }}
   */
  let {
    services = [],
    serviceSamples = {},
    sseClientCount = null,
    version = null,
    uptimeSecs = null,
  } = $props();

  // The console's row, built by the same function the Services list uses, so
  // the two cannot report different figures for the same second.
  let row = $derived(consoleRow(serviceRows(services, serviceSamples)));
  let cards = $derived(consoleDetailCards({ row, uptimeSecs, sseClientCount }));
  let cpuGraph = $derived(rowCpuGraphSpec(row ?? { displayName: 'Trusty Console' }));
  let memGraph = $derived(rowMemoryGraphSpec(row ?? { displayName: 'Trusty Console' }));

  const graphFor = (key) => (key === 'cpu' ? cpuGraph : memGraph);
</script>

<section class="foundry console-details">
  <div class="dash-header">
    <h2 class="dash-title">{consoleHeading(version)}</h2>
    {#if row}
      <Badge tone="success" dot>{row.statusLabel}</Badge>
    {/if}
  </div>

  <div class="stat-grid four">
    {#each cards as card (card.key)}
      <StatCard label={card.label} value={card.value} meta={card.meta}>
        {#if card.graph}
          {#snippet footer()}
            {@const graph = graphFor(card.graph)}
            <BarGraph values={graph.values} max={graph.max} label={graph.label} height="2.25rem" />
          {/snippet}
        {/if}
      </StatCard>
    {/each}
  </div>

  <!-- #6908: one line, not three placeholder widgets. -->
  <p class="deferred">{DEFERRED_LINE}</p>
</section>

<style>
  .console-details {
    margin-bottom: 2rem;
  }

  /* Same header shape as MachineStatusPanel, so the two dashboards read as one
     system rather than two authors. */
  .dash-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .dash-title {
    margin: 0;
    font-family: var(--trusty-display);
    font-size: 1.25rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--trusty-text-primary);
  }

  /* A note, not a fault: nothing is broken, the transport simply is not built.
     Muted secondary text rather than the danger colour the failure notices use. */
  .deferred {
    margin: 0;
    background: var(--trusty-card-bg);
    border: 1.5px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: 0.9rem 1.1rem;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
</style>
