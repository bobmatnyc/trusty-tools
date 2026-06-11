<script>
  /**
   * @typedef {{ id: string, display_name: string, status: string, version?: string, url?: string }} Service
   * @type {{ service: Service, onViewDetails?: (id: string) => void }}
   */
  let { service, onViewDetails } = $props();

  const STATUS_LABELS = {
    running: 'Running',
    available: 'Available',
    absent: 'Absent',
  };

  const STATUS_COLORS = {
    running: '#22c55e',
    available: '#f59e0b',
    absent: '#64748b',
  };

  // Known services that have a details tab in the console.
  const TABBED_SERVICES = new Set([
    'trusty-search',
    'trusty-memory',
    'trusty-analyze',
    'trusty-review',
  ]);

  let statusLabel = $derived(STATUS_LABELS[service.status] ?? service.status);
  let statusColor = $derived(STATUS_COLORS[service.status] ?? '#94a3b8');
  let hasTab = $derived(TABBED_SERVICES.has(service.id));

  function handleViewDetails() {
    onViewDetails?.(service.id);
  }
</script>

<div class="card">
  <div class="card-header">
    <h2 class="name">{service.display_name}</h2>
    <span class="badge" style="background: {statusColor}22; color: {statusColor}; border-color: {statusColor}44;">
      <span class="dot" style="background: {statusColor};"></span>
      {statusLabel}
    </span>
  </div>

  <div class="card-body">
    <p class="id">ID: <code>{service.id}</code></p>
    {#if service.version}
      <p class="version">Version: <code>{service.version}</code></p>
    {/if}
    {#if service.status === 'absent'}
      <p class="hint">Install with <code>cargo install {service.id}</code></p>
    {:else if service.status === 'available'}
      <p class="hint">Binary found but daemon is not running.</p>
    {/if}
    {#if hasTab && onViewDetails}
      <button class="details-btn" onclick={handleViewDetails}>
        View details →
      </button>
    {/if}
  </div>
</div>

<style>
  .card {
    background: #1e2130;
    border: 1px solid #2d3348;
    border-radius: 0.75rem;
    padding: 1.25rem;
    transition: border-color 0.15s;
  }
  .card:hover {
    border-color: #3d4568;
  }
  .card-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }
  .name {
    font-size: 1.1rem;
    font-weight: 600;
    margin: 0;
    color: #e2e8f0;
  }
  .badge {
    display: flex;
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
  }
  .card-body p {
    margin: 0.3rem 0;
    font-size: 0.85rem;
    color: #94a3b8;
  }
  code {
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    font-size: 0.8rem;
    background: #0f1117;
    padding: 0.1rem 0.35rem;
    border-radius: 0.25rem;
    color: #e2e8f0;
  }
  .hint {
    font-style: italic;
  }
  .details-btn {
    margin-top: 0.75rem;
    background: none;
    border: 1px solid #3d4568;
    border-radius: 0.4rem;
    color: #7c3aed;
    cursor: pointer;
    font-size: 0.8rem;
    font-weight: 500;
    padding: 0.3rem 0.75rem;
    transition: background 0.15s, border-color 0.15s;
  }
  .details-btn:hover {
    background: #7c3aed18;
    border-color: #7c3aed;
  }
</style>
