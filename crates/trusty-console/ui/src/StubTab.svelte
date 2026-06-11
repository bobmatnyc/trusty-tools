<script>
  /**
   * Placeholder tab for services whose metrics endpoints are not yet wired
   * (trusty-search — Phase 1, trusty-memory — Phase 2, trusty-review).
   *
   * Shows the current service status/version from /api/console/services and a
   * clear "coming soon" notice.  Never calls daemon HTTP directly.
   *
   * @typedef {{ id: string, display_name: string, status: string, version?: string, url?: string }} Service
   * @type {{ service: Service | null, label: string }}
   */
  let { service, label } = $props();

  // Phase labels so the notice is accurate per service.
  const PHASE_NOTES = {
    'trusty-search':  'Phase 1 of #1104',
    'trusty-memory':  'Phase 2 of #1104',
    'trusty-review':  'a future phase of #1104',
  };

  let phaseNote = $derived(
    service ? (PHASE_NOTES[service.id] ?? 'a future phase') : 'a future phase'
  );

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
  <h2 class="tab-title">{label}</h2>

  <!-- Current status from /api/console/services (always available) -->
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
          <code>{service.version}</code>
        </div>
      {/if}
    {:else}
      <p class="muted">Service not detected on this machine.</p>
    {/if}
  </section>

  <!-- Coming-soon notice -->
  <section class="section stub-notice">
    <p class="notice-text">
      Native metrics for {label} are coming in {phaseNote}.
    </p>
    <p class="notice-sub">
      This tab will surface live metrics fetched via the console's own
      <code>/api/console/metrics/*</code> endpoints — no direct daemon HTTP
      calls from the browser.
    </p>
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
  code {
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
  .stub-notice {
    border-style: dashed;
    border-color: #3d4568;
  }
  .notice-text {
    font-size: 0.9rem;
    font-weight: 500;
    color: #94a3b8;
    margin: 0 0 0.5rem;
  }
  .notice-sub {
    font-size: 0.8rem;
    color: #64748b;
    margin: 0;
    line-height: 1.5;
  }
</style>
