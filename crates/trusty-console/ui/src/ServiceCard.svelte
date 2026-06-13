<script>
  /**
   * Why: Displays a single service's status card; delegates tab-awareness to
   *      the caller (App.svelte) which owns SERVICE_TAB_MAP — the single source
   *      of truth for which services have real detail tabs.
   * What: Renders service name, status badge, optional hint, and a "View details"
   *       button when the service appears in the caller-supplied tabbedServices set.
   * Test: Mount with a service whose id is in tabbedServices and onViewDetails set —
   *       assert button visible; mount without the id in the set — assert no button.
   *
   * @typedef {{ id: string, display_name: string, status: string, version?: string, url?: string, hint?: string }} Service
   * @type {{ service: Service, tabbedServices: Set<string>, onViewDetails?: (id: string) => void }}
   */
  let { service, tabbedServices = new Set(), onViewDetails } = $props();

  const STATUS_LABELS = {
    running: 'Running',
    available: 'Available',
    absent: 'Absent',
    degraded: 'Degraded',
  };

  // Maps a service status to a theme-adaptive CSS custom property reference.
  // The returned `var(--color-status-*)` resolves against the active palette at
  // render time, so badges recolor automatically when the theme flips — no JS hex.
  const STATUS_VARS = {
    running: 'var(--color-status-ok)',
    available: 'var(--color-status-warn)',
    absent: 'var(--color-status-absent)',
    // Degraded uses orange to indicate partial impairment (reachable but the
    // console_metrics tool is missing). Distinct from the amber "Available" state.
    degraded: 'var(--color-status-degraded)',
  };

  let statusLabel = $derived(STATUS_LABELS[service.status] ?? service.status);
  let statusVar = $derived(STATUS_VARS[service.status] ?? 'var(--color-text-muted)');
  // hasTab is derived from the caller-owned tabbedServices — no local duplicate.
  let hasTab = $derived(tabbedServices.has(service.id));

  function handleViewDetails() {
    onViewDetails?.(service.id);
  }
</script>

<div class="card">
  <div class="card-header">
    <h2 class="name">{service.display_name}</h2>
    <span class="badge" style="--_s: {statusVar};">
      <span class="dot"></span>
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
    {:else if service.status === 'degraded'}
      <p class="hint degraded-hint">{service.hint ?? 'Service reachable but its metrics tool is unavailable.'}</p>
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
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: 0.75rem;
    padding: 1.25rem;
    transition: border-color 0.15s;
  }
  .card:hover {
    border-color: var(--color-border-hover);
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
    color: var(--color-text-primary);
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
    /* --_s is supplied inline (statusVar) as a theme-adaptive --color-status-* ref.
       The 13%/27% color-mix produces the subtle bg/border tint on either palette. */
    --_s: var(--color-text-muted);
    color: var(--_s);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--_s);
  }
  .card-body p {
    margin: 0.3rem 0;
    font-size: 0.85rem;
    color: var(--color-text-secondary);
  }
  code {
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    font-size: 0.8rem;
    background: var(--color-surface-code);
    padding: 0.1rem 0.35rem;
    border-radius: 0.25rem;
    color: var(--color-text-primary);
  }
  .hint {
    font-style: italic;
  }
  .degraded-hint {
    color: var(--color-status-degraded);
  }
  .details-btn {
    margin-top: 0.75rem;
    background: none;
    border: 1px solid var(--color-border-hover);
    border-radius: 0.4rem;
    color: var(--color-accent);
    cursor: pointer;
    font-size: 0.8rem;
    font-weight: 500;
    padding: 0.3rem 0.75rem;
    transition: background 0.15s, border-color 0.15s;
  }
  .details-btn:hover {
    background: color-mix(in srgb, var(--color-accent) 9%, transparent);
    border-color: var(--color-accent);
  }
</style>
