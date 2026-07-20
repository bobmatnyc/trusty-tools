<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';

  /** Format bytes into a human-readable string (KB / MB / GB). */
  function formatBytes(bytes) {
    if (bytes == null) return '—';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);

  /**
   * Why: Fetches search metrics while preventing concurrent in-flight requests
   *      from stacking (e.g. slow >20 s fetch overlapping the next interval tick
   *      or a rapid manual button click).
   * What: Returns early when a fetch is already in progress; otherwise sets the
   *       appropriate loading flag, fetches /api/console/metrics/search, and
   *       stores the result or an error message.
   * Test: Call twice in rapid succession — assert only one HTTP request is made
   *       and state is consistent after both calls resolve.
   */
  async function fetchMetrics(isRefresh = false) {
    // Guard: drop the tick if a fetch is already in flight.
    if (refreshing || (isRefresh && loading)) return;

    if (isRefresh) {
      refreshing = true;
    } else {
      loading = true;
    }
    try {
      const resp = await fetch('/api/console/metrics/search');
      if (resp.status === 503) {
        error = 'trusty-search metrics not yet available (daemon absent or first boot).';
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

  // Theme-adaptive CSS custom property refs (resolved against the active palette
  // at render time) instead of hardcoded hex — badges/stats recolor on theme flip.
  let statusVar = $derived(
    report?.status === 'ok'         ? 'var(--trusty-success)'
    : report?.status === 'degraded' ? 'var(--trusty-warning)'
    : 'var(--trusty-danger)'
  );

</script>

<div class="tab-content">
  <RefreshHeader title="Trusty Search" onRefresh={() => fetchMetrics(true)} {refreshing} />

  {#if loading}
    <div class="placeholder">Loading search metrics…</div>
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
        <span class="stat-value">{report.metrics?.index_count ?? 0}</span>
        <span class="stat-label">Indexes</span>
      </div>
      <div class="stat-card" class:degraded={report.metrics?.warm_boot_degraded}>
        <span class="stat-value">
          {report.metrics?.warm_boot_degraded ? 'Yes' : 'No'}
        </span>
        <span class="stat-label">Warm Boot Degraded</span>
      </div>
    </div>

    <!-- Per-index table -->
    {#if report.metrics?.indexes?.length > 0}
      <h3 class="sub-title">Indexes</h3>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Root Path</th>
              <th>Size</th>
            </tr>
          </thead>
          <tbody>
            {#each report.metrics.indexes as idx (idx.id)}
              <tr>
                <td><code>{idx.id ?? '—'}</code></td>
                <td class="path">{idx.root_path ?? '—'}</td>
                <td class="num">
                  {idx.size_bytes != null ? formatBytes(idx.size_bytes) : '—'}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="empty-hint">No indexes registered.</p>
    {/if}
  {/if}
</div>

<style>
  .tab-content { padding: 0.25rem 0; }
  .placeholder, .not-available {
    background: var(--trusty-card-bg); border-radius: 0.5rem;
    padding: 1.25rem; color: var(--trusty-text-secondary); font-size: 0.9rem;
  }
  .not-available { color: var(--trusty-warning); }

  .meta-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem;
  }
  /* --_s supplied inline (statusVar) as a theme-adaptive --trusty-status-* ref. */
  .badge {
    display: inline-flex; align-items: center; gap: 0.35rem;
    font-size: 0.75rem; font-weight: 600; padding: 0.2rem 0.6rem;
    border-radius: 9999px; border: 1px solid;
    --_s: var(--trusty-text-muted);
    color: var(--_s);
    background: rgba(0,0,0,0.08);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: rgba(0,0,0,0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--_s); }
  .version { color: var(--trusty-text-secondary); font-size: 0.85rem; }

  .stat-grid {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; align-items: center; gap: 0.25rem;
  }
  .stat-value { font-size: 1.6rem; font-weight: 700; color: var(--trusty-text-primary); }
  .stat-label { font-size: 0.75rem; color: var(--trusty-text-secondary); text-transform: uppercase; letter-spacing: 0.05em; }
  /* .degraded modifier: warm-boot-degraded card gets amber border/value tint,
     consistent with the --_s badge pattern (single scoped var, no inline one-offs). */
  .stat-card.degraded {
    --_s: var(--trusty-warning);
    border-color: rgba(0,0,0,0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .stat-card.degraded .stat-value { color: var(--trusty-warning); }

  .sub-title { font-size: 1rem; font-weight: 600; color: var(--trusty-text-secondary); margin: 0 0 0.75rem; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  th {
    text-align: left; padding: 0.5rem 0.75rem;
    background: var(--trusty-card-bg); color: var(--trusty-text-secondary); font-weight: 600;
    border-bottom: 1px solid var(--trusty-border);
  }
  td { padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--trusty-card-bg); color: var(--trusty-text-primary); }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: var(--trusty-card-bg); }
  td.num { text-align: right; font-variant-numeric: tabular-nums; }
  td.path { font-size: 0.8rem; color: var(--trusty-text-secondary); max-width: 400px; overflow: hidden; text-overflow: ellipsis; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    background: var(--trusty-surface-raised); padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }
  .empty-hint { color: var(--trusty-text-secondary); font-size: 0.85rem; }
</style>
