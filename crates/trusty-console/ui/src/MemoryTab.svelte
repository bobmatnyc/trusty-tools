<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);

  /**
   * Why: Fetches memory metrics while preventing concurrent in-flight requests
   *      from stacking (e.g. slow >20 s fetch overlapping the next interval tick
   *      or a rapid manual button click).
   * What: Returns early when a fetch is already in progress; otherwise sets the
   *       appropriate loading flag, fetches /api/console/metrics/memory, and
   *       stores the result or an error message.
   * Test: Call twice in rapid succession — assert only one HTTP request is made
   *       and state is consistent after both calls resolve.
   */
  async function fetchMetrics(isRefresh = false) {
    // Guard: drop the tick if a fetch is already in flight.
    // The very first call has refreshing=false and loading=true so it always
    // proceeds; subsequent interval ticks are dropped while busy.
    if (refreshing || (isRefresh && loading)) return;

    if (isRefresh) {
      refreshing = true;
    } else {
      loading = true;
    }
    try {
      const resp = await fetch('/api/console/metrics/memory');
      if (resp.status === 503) {
        error = 'trusty-memory metrics not yet available (daemon absent or first boot).';
        return;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      report = await resp.json();
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  /** Auto-refresh interval handle — cleared on component destroy to prevent leaks. */
  let refreshInterval;

  onMount(async () => {
    await fetchMetrics();
    refreshInterval = setInterval(() => fetchMetrics(true), 20_000);
  });

  onDestroy(() => {
    clearInterval(refreshInterval);
  });

  // Theme-adaptive CSS custom property ref (resolved against the active palette
  // at render time) instead of hardcoded hex — badge recolors on theme flip.
  let statusVar = $derived(
    report?.status === 'ok'       ? 'var(--color-status-ok)'
    : report?.status === 'degraded' ? 'var(--color-status-warn)'
    : 'var(--color-status-error)'
  );
</script>

<div class="tab-content">
  <RefreshHeader title="Trusty Memory" onRefresh={() => fetchMetrics(true)} {refreshing} />

  {#if loading}
    <div class="placeholder">Loading memory metrics…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else if report}
    <!-- Status badge + version -->
    <div class="meta-row">
      <span class="badge" style="--_s: {statusVar};">
        <span class="dot"></span>
        {report.status}
      </span>
      <span class="version">v{report.version}</span>
    </div>

    <!-- Aggregate stats -->
    <div class="stat-grid">
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.palace_count ?? 0}</span>
        <span class="stat-label">Palaces</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_drawers ?? 0}</span>
        <span class="stat-label">Total Drawers</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_vectors ?? 0}</span>
        <span class="stat-label">Total Vectors</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_kg_triples ?? 0}</span>
        <span class="stat-label">KG Triples</span>
      </div>
    </div>

    <!-- Per-palace table -->
    {#if report.metrics?.palaces?.length > 0}
      <h3 class="sub-title">Palaces (top {report.metrics.palaces.length})</h3>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Name</th>
              <th>Drawers</th>
              <th>Vectors</th>
              <th>KG Triples</th>
            </tr>
          </thead>
          <tbody>
            {#each report.metrics.palaces as p (p.id)}
              <tr>
                <td><code>{p.id}</code></td>
                <td>{p.name}</td>
                <td class="num">{p.drawer_count}</td>
                <td class="num">{p.vector_count}</td>
                <td class="num">{p.kg_triple_count}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="empty-hint">No palaces found.</p>
    {/if}
  {/if}
</div>

<style>
  .tab-content { padding: 0.25rem 0; }
  .placeholder, .not-available {
    background: var(--color-surface); border-radius: 0.5rem;
    padding: 1.25rem; color: var(--color-text-secondary); font-size: 0.9rem;
  }
  .not-available { color: var(--color-status-warn); }

  .meta-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem;
  }
  /* --_s supplied inline (statusVar) as a theme-adaptive --color-status-* ref. */
  .badge {
    display: inline-flex; align-items: center; gap: 0.35rem;
    font-size: 0.75rem; font-weight: 600; padding: 0.2rem 0.6rem;
    border-radius: 9999px; border: 1px solid;
    --_s: var(--color-text-muted);
    color: var(--_s);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--_s); }
  .version { color: var(--color-text-secondary); font-size: 0.85rem; }

  .stat-grid {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: var(--color-surface); border: 1px solid var(--color-border); border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; align-items: center; gap: 0.25rem;
  }
  .stat-value { font-size: 1.6rem; font-weight: 700; color: var(--color-text-primary); }
  .stat-label { font-size: 0.75rem; color: var(--color-text-secondary); text-transform: uppercase; letter-spacing: 0.05em; }

  .sub-title { font-size: 1rem; font-weight: 600; color: var(--color-text-secondary); margin: 0 0 0.75rem; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  th {
    text-align: left; padding: 0.5rem 0.75rem;
    background: var(--color-surface); color: var(--color-text-secondary); font-weight: 600;
    border-bottom: 1px solid var(--color-border);
  }
  td { padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--color-surface); color: var(--color-text-primary); }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: var(--color-surface); }
  td.num { text-align: right; font-variant-numeric: tabular-nums; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    background: var(--color-surface-code); padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }
  .empty-hint { color: var(--color-text-secondary); font-size: 0.85rem; }
</style>
