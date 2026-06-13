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

  // Theme-adaptive CSS custom property refs (resolved against the active palette
  // at render time) instead of hardcoded hex — badge/stat recolor on theme flip.
  let statusVar = $derived(
    report?.status === 'ok'         ? 'var(--color-status-ok)'
    : report?.status === 'degraded' ? 'var(--color-status-warn)'
    : 'var(--color-status-error)'
  );

  let inferenceVar = $derived(
    report?.metrics?.inference === 'ok' ? 'var(--color-status-ok)' : 'var(--color-status-warn)'
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
      <span class="badge" style="--_s: {statusVar};">
        <span class="dot"></span>
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
        <span class="stat-value" style="color: {inferenceVar};">
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
    background: var(--color-surface); border-radius: 0.5rem;
    padding: 1.25rem; color: var(--color-text-secondary); font-size: 0.9rem;
  }
  .not-available { color: var(--color-status-warn); }

  .meta-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem; flex-wrap: wrap;
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
  .dry-run-badge {
    font-size: 0.7rem; font-weight: 600; padding: 0.15rem 0.5rem;
    border-radius: 9999px;
    background: color-mix(in srgb, var(--color-accent) 13%, transparent);
    color: var(--color-accent-light);
    border: 1px solid color-mix(in srgb, var(--color-accent) 27%, transparent);
  }

  .stat-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: var(--color-surface); border: 1px solid var(--color-border); border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; gap: 0.35rem;
  }
  .stat-card.wide {
    grid-column: span 2;
  }
  .stat-value {
    font-size: 1rem; font-weight: 600; color: var(--color-text-primary); word-break: break-word;
  }
  .stat-value.model {
    font-family: 'JetBrains Mono', monospace; font-size: 0.85rem;
  }
  .stat-label {
    font-size: 0.75rem; color: var(--color-text-secondary); text-transform: uppercase; letter-spacing: 0.05em;
  }

  .sub-title { font-size: 1rem; font-weight: 600; color: var(--color-text-secondary); margin: 0 0 0.75rem; }

  .dep-list {
    display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 1.5rem;
  }
  .dep-row {
    display: flex; align-items: center; gap: 0.6rem;
    background: var(--color-surface); border: 1px solid var(--color-border); border-radius: 0.4rem;
    padding: 0.6rem 0.9rem;
  }
  .dep-name { font-weight: 500; color: var(--color-text-primary); font-size: 0.9rem; }
  .dep-required { color: var(--color-text-muted); font-size: 0.75rem; }
  /* Dependency reachability pills: --_s is a static theme-adaptive status var. */
  .dep-status {
    margin-left: auto; font-size: 0.75rem; font-weight: 600;
    padding: 0.15rem 0.5rem; border-radius: 9999px; border: 1px solid;
    --_s: var(--color-text-muted);
    color: var(--_s);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dep-status.ok   { --_s: var(--color-status-ok);    }
  .dep-status.fail { --_s: var(--color-status-error); }
  .dep-status.warn { --_s: var(--color-text-muted);   }
</style>
