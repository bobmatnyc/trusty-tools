<!--
  The Overview tab's Foundry machine-status dashboard (#6518, phase 2 of #6516).

  Why: the Overview tab showed per-service cards and nothing about the machine
  those services run on. This panel renders the host half of the phase-1
  `GET /api/console/machine-status` payload as the Foundry dashboard layout
  (docs/design/UI/design-system-svelte/src/screens/search/Dashboard.svelte): a
  four-card host row, each card carrying a 1 s history bar graph on its bottom
  edge.
  What: a pure renderer over `machineStatus.js` — every unit conversion, tone
  lookup and fetch branch lives there, so this file holds only markup, the poll
  timer, and the three render states (loading / message / dashboard).

  #6642 changed two things. The card values now prefer the newest sample from
  the caller-owned machine-status stream, so the headline number and the
  rightmost bar are the same second; the 15 s fetch stays as the cold-start
  paint and the fallback for a browser that cannot hold an EventSource open.
  And the rollup services table is gone: `ServicesList.svelte` is now the one
  services section on the page, per the owner's ruling.
  Test: `machineStatus.test.js` covers everything rendered here, including
  `hostGraphs`; the panel itself is verified by the binary smoke run against a
  live daemon.
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import StatCard from './StatCard.svelte';
  import Badge from './Badge.svelte';
  import BarGraph from './BarGraph.svelte';
  import {
    DISK_THRESHOLDS,
    PCT_THRESHOLDS,
    windowMax,
  } from './barGraph.js';
  import {
    fetchMachineStatus,
    formatRateMBs,
    hostGraphs,
    pressureTone,
    statCards,
    toneLabel,
  } from './machineStatus.js';

  /**
   * @type {{ samples?: object[] }} the caller's host history ring, oldest
   * first. `App.svelte` owns the single EventSource; this panel only draws it.
   */
  let { samples = [] } = $props();

  // The server samples on its own `--poll-interval`. This fetch is the cold
  // paint and the no-EventSource fallback; the stream carries the live values.
  const POLL_MS = 15_000;

  let status = $state(null);
  let error = $state(null);
  let cold = $state(false);
  let loading = $state(true);
  let inFlight = false;

  /**
   * Read the machine status once, dropping the tick if one is already in
   * flight — the same guard the per-service tabs use, so a slow response
   * cannot stack requests behind the interval.
   */
  async function load() {
    if (inFlight) return;
    inFlight = true;
    try {
      const result = await fetchMachineStatus();
      status = result.status;
      error = result.error;
      cold = result.cold;
    } finally {
      inFlight = false;
      loading = false;
    }
  }

  let timer;
  onMount(async () => {
    await load();
    timer = setInterval(load, POLL_MS);
  });
  onDestroy(() => clearInterval(timer));

  // #6642: the stream's newest sample wins. A `sample` event is a whole
  // `HostMetrics`, the same shape `status.host` carries, so the cards below
  // need no branch of their own.
  let host = $derived(samples.length > 0 ? samples[samples.length - 1] : status?.host);

  // `statCards(null)` yields the four cards with placeholder values, which is
  // what the cold-cache state renders — the grid keeps its shape so a pending
  // first sample does not look like a failure.
  let cards = $derived(statCards(host));
  let graphs = $derived(hostGraphs(samples));

  /**
   * The bar-graph settings for one card, keyed by `statCards`' `key`.
   *
   * CPU and memory band at 80/95 and disk at 85/95 — the same
   * `HostThresholds::default()` numbers the pressure badge above the graph
   * already uses, so a badge reading WARNING cannot sit over green bars.
   * Network is a rate with no band; it is scaled to the busiest second in the
   * window, and its label says so because a bar height alone would be read as
   * a fraction of some fixed capacity.
   */
  function graphFor(key) {
    const values = graphs[key] ?? [];
    if (key === 'network') {
      const max = windowMax(values, 1);
      return {
        values,
        max,
        thresholds: null,
        label: `Total throughput, one bar per second, peak ${formatRateMBs(max)}`,
      };
    }
    return {
      values,
      max: 100,
      thresholds: key === 'disk' ? DISK_THRESHOLDS : PCT_THRESHOLDS,
      label: `${key} usage %, one bar per second`,
    };
  }
</script>

<section class="foundry machine-status">
  <div class="dash-header">
    <h2 class="dash-title">Machine Status</h2>
    {#if host}
      <Badge tone={pressureTone(host.overall_pressure)} dot>
        {toneLabel(host.overall_pressure)}
      </Badge>
    {/if}
  </div>

  {#if loading}
    <div class="notice">Sampling host metrics…</div>
  {:else if error && !host}
    <div class="notice" class:pending={cold} class:failed={!cold}>{error}</div>
  {/if}

  {#if !loading}
    <div class="stat-grid four">
      {#each cards as card (card.key)}
        {@const graph = graphFor(card.key)}
        <StatCard label={card.label} value={card.value} meta={card.meta}>
          {#if card.badge}
            <div class="card-badge"><Badge tone={card.tone}>{card.badge}</Badge></div>
          {/if}
          {#if card.extra}<div class="card-extra">{card.extra}</div>{/if}
          {#snippet footer()}
            <!-- #6642: one bar per 1 s sample, newest at the right. -->
            <BarGraph
              values={graph.values}
              max={graph.max}
              thresholds={graph.thresholds}
              label={graph.label}
              height="2.25rem"
            />
          {/snippet}
        </StatCard>
      {/each}
    </div>
  {/if}
</section>

<style>
  .machine-status { margin-bottom: 2rem; }

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

  .notice {
    background: var(--trusty-card-bg);
    border: 1.5px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: 0.9rem 1.1rem;
    margin-bottom: var(--trusty-space-4);
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
  /* A cold cache is a pending first sample, not a fault — it reads as a note.
     Anything else is a real failure and takes the danger colour. */
  .notice.pending { color: var(--trusty-text-secondary); }
  .notice.failed { color: var(--trusty-danger); border-color: var(--trusty-danger); }

  .card-badge { margin-top: 6px; }
  .card-extra {
    margin-top: 6px;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary);
    font-family: var(--trusty-mono);
  }
</style>
