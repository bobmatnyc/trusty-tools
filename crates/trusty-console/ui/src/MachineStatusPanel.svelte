<!--
  The Overview tab's Foundry machine-status dashboard (#6518, phase 2 of #6516).

  Why: the Overview tab showed per-service cards and nothing about the machine
  those services run on. This panel renders the phase-1
  `GET /api/console/machine-status` payload as the Foundry dashboard layout
  (docs/design/UI/design-system-svelte/src/screens/search/Dashboard.svelte): a
  four-card host row, then one rollup card listing every reporting service.
  What: a pure renderer over `machineStatus.js` — every unit conversion, tone
  lookup and fetch branch lives there, so this file holds only markup, the poll
  timer, and the three render states (loading / message / dashboard). It sits
  ABOVE the existing ServiceCard grid rather than replacing it: this rollup can
  only list services that produced a report, so a never-installed or absent
  service appears in that grid and nowhere else.
  Test: `machineStatus.test.js` covers everything rendered here; the panel
  itself is verified by the binary smoke run against a live daemon.
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import StatCard from './StatCard.svelte';
  import Badge from './Badge.svelte';
  import {
    fetchMachineStatus,
    pressureTone,
    rollupTone,
    serviceRows,
    statCards,
    toneLabel,
  } from './machineStatus.js';

  // The server samples on its own `--poll-interval`, 15s by default. Polling
  // faster only re-reads the same cached snapshot.
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

  // `statCards(null)` yields the four cards with placeholder values, which is
  // what the cold-cache state renders — the grid keeps its shape so a pending
  // first sample does not look like a failure.
  let cards = $derived(statCards(status?.host));
  let rollup = $derived(status?.services ?? null);
  let rows = $derived(serviceRows(status));
</script>

<section class="foundry machine-status">
  <div class="dash-header">
    <h2 class="dash-title">Machine Status</h2>
    {#if status?.host}
      <Badge tone={pressureTone(status.host.overall_pressure)} dot>
        {toneLabel(status.host.overall_pressure)}
      </Badge>
    {/if}
  </div>

  {#if loading}
    <div class="notice">Sampling host metrics…</div>
  {:else if error}
    <div class="notice" class:pending={cold} class:failed={!cold}>{error}</div>
  {/if}

  {#if !loading}
    <div class="stat-grid four">
      {#each cards as card (card.key)}
        <StatCard label={card.label} value={card.value} meta={card.meta}>
          {#if card.badge}
            <div class="card-badge"><Badge tone={card.tone}>{card.badge}</Badge></div>
          {/if}
          {#if card.extra}<div class="card-extra">{card.extra}</div>{/if}
        </StatCard>
      {/each}
    </div>

    <div class="card">
      <div class="card-header header-row">
        <span>Services</span>
        {#if rollup}
          <Badge tone={rollupTone(rollup)}>{rollup.total} reporting</Badge>
        {/if}
      </div>
      <div class="card-body counts">
        <span class="count"><b>{rollup?.total ?? 0}</b> total</span>
        <span class="count ok"><b>{rollup?.ok ?? 0}</b> ok</span>
        <span class="count degraded"><b>{rollup?.degraded ?? 0}</b> degraded</span>
        <span class="count error"><b>{rollup?.error ?? 0}</b> error</span>
      </div>
      {#if rows.length > 0}
        <div class="table-wrap">
          <table class="table">
            <thead>
              <tr>
                <th>Service</th>
                <th>Version</th>
                <th>Status</th>
                <th>Collected</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as row (row.id)}
                <tr>
                  <td class="name">{row.displayName}</td>
                  <td class="mono">{row.version}</td>
                  <td><Badge tone={row.tone}>{row.healthLabel}</Badge></td>
                  <td class="mono collected">{row.collected}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else}
        <p class="empty">No service has reported metrics yet.</p>
      {/if}
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

  .header-row { display: flex; align-items: center; justify-content: space-between; }

  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--trusty-space-5);
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
    border-bottom: 1px solid var(--trusty-border);
  }
  .count b {
    font-family: var(--trusty-display);
    font-size: var(--trusty-fs-lg);
    color: var(--trusty-text-primary);
    margin-right: 0.35rem;
  }
  .count.ok b { color: var(--trusty-success); }
  .count.degraded b { color: var(--trusty-warning); }
  .count.error b { color: var(--trusty-danger); }

  .table-wrap { overflow-x: auto; }
  .name { font-weight: 600; color: var(--trusty-text-primary); }
  .mono { font-family: var(--trusty-mono); font-size: var(--trusty-fs-xs); }
  .collected { color: var(--trusty-text-muted); }
  .empty {
    margin: 0;
    padding: var(--trusty-space-5);
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
</style>
