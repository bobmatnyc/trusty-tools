<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);

  /**
   * Why: Fetches review metrics while preventing concurrent in-flight requests
   *      from stacking (e.g. slow >20 s fetch overlapping the next interval tick
   *      or a rapid manual button click).
   * What: Returns early when a fetch is already in progress; otherwise sets the
   *       appropriate loading flag, fetches /api/console/metrics/review, and
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
      const resp = await fetch('/api/console/metrics/review');
      if (resp.status === 503) {
        error = 'trusty-review metrics not yet available (daemon absent or first boot).';
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

  let statusColor = $derived(
    report?.status === 'ok'         ? '#22c55e'
    : report?.status === 'degraded' ? '#f59e0b'
    : '#ef4444'
  );

  let inferenceColor = $derived(
    report?.metrics?.inference === 'ok' ? '#22c55e' : '#f59e0b'
  );
</script>

<div class="tab-content">
  <RefreshHeader title="Trusty Review" onRefresh={() => fetchMetrics(true)} {refreshing} />

  {#if loading}
    <div class="placeholder">Loading review metrics…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else if report}
    <!-- Status badge + version -->
    <div class="meta-row">
      <span class="badge" style="background: {statusColor}22; color: {statusColor}; border-color: {statusColor}44;">
        <span class="dot" style="background: {statusColor};"></span>
        {report.status}
      </span>
      <span class="version">v{report.version}</span>
      {#if report.metrics?.dry_run}
        <span class="dry-run-badge">dry-run</span>
      {/if}
    </div>

    <!-- Stat grid: model + inference + deps -->
    <div class="stat-grid">
      <div class="stat-card wide">
        <span class="stat-label">Reviewer Model</span>
        <span class="stat-value model">{report.metrics?.reviewer_model ?? '—'}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">Inference</span>
        <span class="stat-value" style="color: {inferenceColor};">
          {report.metrics?.inference ?? '—'}
        </span>
      </div>
    </div>

    <!-- Dependency reachability -->
    <h3 class="sub-title">Dependencies</h3>
    <div class="dep-list">
      <div class="dep-row">
        <span class="dep-name">trusty-search</span>
        <span class="dep-required">(required)</span>
        {#if report.metrics?.search_reachable}
          <span class="dep-status ok">reachable</span>
        {:else}
          <span class="dep-status fail">unreachable</span>
        {/if}
      </div>
      <div class="dep-row">
        <span class="dep-name">trusty-analyze</span>
        <span class="dep-required">(optional)</span>
        {#if report.metrics?.analyze_reachable}
          <span class="dep-status ok">reachable</span>
        {:else}
          <span class="dep-status warn">not available</span>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .tab-content { padding: 0.25rem 0; }
  .placeholder, .not-available {
    background: #1e2130; border-radius: 0.5rem;
    padding: 1.25rem; color: #94a3b8; font-size: 0.9rem;
  }
  .not-available { color: #f59e0b; }

  .meta-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem; flex-wrap: wrap;
  }
  .badge {
    display: inline-flex; align-items: center; gap: 0.35rem;
    font-size: 0.75rem; font-weight: 600; padding: 0.2rem 0.6rem;
    border-radius: 9999px; border: 1px solid;
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; }
  .version { color: #94a3b8; font-size: 0.85rem; }
  .dry-run-badge {
    font-size: 0.7rem; font-weight: 600; padding: 0.15rem 0.5rem;
    border-radius: 9999px; background: #7c3aed22; color: #a78bfa;
    border: 1px solid #7c3aed44;
  }

  .stat-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; gap: 0.35rem;
  }
  .stat-card.wide {
    grid-column: span 2;
  }
  .stat-value {
    font-size: 1rem; font-weight: 600; color: #e2e8f0; word-break: break-word;
  }
  .stat-value.model {
    font-family: 'JetBrains Mono', monospace; font-size: 0.85rem;
  }
  .stat-label {
    font-size: 0.75rem; color: #94a3b8; text-transform: uppercase; letter-spacing: 0.05em;
  }

  .sub-title { font-size: 1rem; font-weight: 600; color: #94a3b8; margin: 0 0 0.75rem; }

  .dep-list {
    display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 1.5rem;
  }
  .dep-row {
    display: flex; align-items: center; gap: 0.6rem;
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.4rem;
    padding: 0.6rem 0.9rem;
  }
  .dep-name { font-weight: 500; color: #e2e8f0; font-size: 0.9rem; }
  .dep-required { color: #64748b; font-size: 0.75rem; }
  .dep-status {
    margin-left: auto; font-size: 0.75rem; font-weight: 600;
    padding: 0.15rem 0.5rem; border-radius: 9999px; border: 1px solid;
  }
  .dep-status.ok  { color: #22c55e; background: #22c55e22; border-color: #22c55e44; }
  .dep-status.fail { color: #ef4444; background: #ef444422; border-color: #ef444444; }
  .dep-status.warn { color: #94a3b8; background: #94a3b822; border-color: #94a3b844; }
</style>
