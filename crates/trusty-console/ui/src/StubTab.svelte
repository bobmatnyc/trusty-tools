<script>
  import { onMount } from 'svelte';

  /** @type {{ name: string, endpoint: string | null }} */
  let { name, endpoint } = $props();

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);

  onMount(async () => {
    if (!endpoint) {
      loading = false;
      return;
    }
    try {
      const resp = await fetch(endpoint);
      if (resp.status === 503) {
        error = `${name} metrics not yet available (daemon absent or first boot).`;
        return;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      report = await resp.json();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  });

  let statusColor = $derived(
    report?.status === 'ok'         ? '#22c55e'
    : report?.status === 'degraded' ? '#f59e0b'
    : '#ef4444'
  );
</script>

<div class="tab-content">
  <h2 class="section-title">Trusty {name}</h2>

  {#if !endpoint}
    <div class="placeholder">Dashboard coming soon for {name}.</div>
  {:else if loading}
    <div class="placeholder">Loading {name} metrics…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else if report}
    <div class="meta-row">
      <span class="badge" style="background: {statusColor}22; color: {statusColor}; border-color: {statusColor}44;">
        <span class="dot" style="background: {statusColor};"></span>
        {report.status}
      </span>
      <span class="version">v{report.version}</span>
    </div>
    <pre class="metrics-dump">{JSON.stringify(report.metrics, null, 2)}</pre>
  {/if}
</div>

<style>
  .tab-content { padding: 0.25rem 0; }
  .section-title {
    font-size: 1.25rem; font-weight: 600; margin: 0 0 1rem; color: #e2e8f0;
  }
  .placeholder, .not-available {
    background: #1e2130; border-radius: 0.5rem;
    padding: 1.25rem; color: #94a3b8; font-size: 0.9rem;
  }
  .not-available { color: #f59e0b; }
  .meta-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem;
  }
  .badge {
    display: inline-flex; align-items: center; gap: 0.35rem;
    font-size: 0.75rem; font-weight: 600; padding: 0.2rem 0.6rem;
    border-radius: 9999px; border: 1px solid;
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; }
  .version { color: #94a3b8; font-size: 0.85rem; }
  .metrics-dump {
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.5rem;
    padding: 1rem; font-size: 0.8rem; color: #94a3b8;
    overflow: auto; max-height: 400px;
    font-family: 'JetBrains Mono', monospace;
  }
</style>
