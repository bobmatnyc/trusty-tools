<script>
  import { onMount } from 'svelte';

  /**
   * @typedef {{ id: string, display_name: string, status: string, version?: string, url?: string }} Service
   * @type {{ service: Service | null }}
   */
  let { service } = $props();

  // Metrics fetched from the console's own API (never from the daemon port).
  let metrics = $state(null);
  let metricsLoading = $state(true);
  let metricsError = $state(null);

  onMount(async () => {
    try {
      const resp = await fetch('/api/console/metrics/analyze');
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      metrics = await resp.json();
    } catch (e) {
      metricsError = e.message;
    } finally {
      metricsLoading = false;
    }
  });

  // Derived status badge
  let statusLabel = $derived(
    service?.status === 'running' ? 'Running'
    : service?.status === 'available' ? 'Available'
    : 'Absent'
  );
  let statusColor = $derived(
    service?.status === 'running' ? '#22c55e'
    : service?.status === 'available' ? '#f59e0b'
    : '#64748b'
  );
</script>

<div class="tab">
  <h2 class="tab-title">Trusty Analyze</h2>

  <!-- Service status row from /api/console/services -->
  <section class="section">
    <h3 class="section-title">Service Status</h3>
    {#if service}
      <div class="stat-row">
        <span class="stat-label">Status</span>
        <span class="badge" style="background: {statusColor}22; color: {statusColor}; border-color: {statusColor}44;">
          <span class="dot" style="background: {statusColor};"></span>
          {statusLabel}
        </span>
      </div>
      {#if service.version}
        <div class="stat-row">
          <span class="stat-label">Version</span>
          <code class="stat-value">{service.version}</code>
        </div>
      {/if}
    {:else}
      <p class="muted">Service not detected.</p>
    {/if}
  </section>

  <!-- Live metrics from /api/console/metrics/analyze -->
  <section class="section">
    <h3 class="section-title">Live Metrics</h3>
    {#if metricsLoading}
      <p class="muted">Loading metrics…</p>
    {:else if metricsError}
      <p class="error">Failed to load metrics: {metricsError}</p>
    {:else if metrics && !metrics.reachable}
      <p class="muted">trusty-analyze is not reachable. Start the daemon to see metrics.</p>
    {:else if metrics}
      <div class="metrics-grid">
        {#if metrics.version}
          <div class="metric-card">
            <span class="metric-label">Daemon version</span>
            <code class="metric-value">{metrics.version}</code>
          </div>
        {/if}

        <div class="metric-card">
          <span class="metric-label">Search backend</span>
          <span class="metric-value"
            class:ok={metrics.search_reachable}
            class:warn={metrics.search_reachable === false}
          >
            {#if metrics.search_reachable === true}
              Connected
            {:else if metrics.search_reachable === false}
              Unreachable
            {:else}
              Unknown
            {/if}
          </span>
        </div>

        {#if metrics.index_count != null}
          <div class="metric-card">
            <span class="metric-label">Visible indexes</span>
            <span class="metric-value">{metrics.index_count}</span>
          </div>
        {/if}
      </div>
    {/if}
  </section>
</div>

<style>
  .tab {
    padding: 0;
  }
  .tab-title {
    font-size: 1.3rem;
    font-weight: 700;
    margin: 0 0 1.25rem;
    color: #e2e8f0;
  }
  .section {
    background: #1e2130;
    border: 1px solid #2d3348;
    border-radius: 0.75rem;
    padding: 1.25rem;
    margin-bottom: 1rem;
  }
  .section-title {
    font-size: 0.8rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: #64748b;
    margin: 0 0 0.85rem;
  }
  .stat-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.5rem;
    font-size: 0.85rem;
  }
  .stat-label {
    color: #94a3b8;
    min-width: 5rem;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.2rem 0.6rem;
    border-radius: 9999px;
    border: 1px solid;
    white-space: nowrap;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  code, .stat-value {
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    font-size: 0.8rem;
    background: #0f1117;
    padding: 0.1rem 0.35rem;
    border-radius: 0.25rem;
    color: #e2e8f0;
  }
  .muted {
    color: #64748b;
    font-size: 0.85rem;
    margin: 0;
  }
  .error {
    color: #f87171;
    font-size: 0.85rem;
    margin: 0;
  }
  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 0.75rem;
  }
  .metric-card {
    background: #0f1117;
    border-radius: 0.5rem;
    padding: 0.85rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .metric-label {
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #64748b;
  }
  .metric-value {
    font-size: 1rem;
    font-weight: 600;
    color: #e2e8f0;
    background: none;
    padding: 0;
  }
  .metric-value.ok {
    color: #22c55e;
  }
  .metric-value.warn {
    color: #f87171;
  }
</style>
