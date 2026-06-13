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

  // Theme-adaptive CSS custom property ref (resolved against the active palette
  // at render time) instead of hardcoded hex — badge recolors on theme flip.
  let statusVar = $derived(
    report?.status === 'ok'         ? 'var(--color-status-ok)'
    : report?.status === 'degraded' ? 'var(--color-status-warn)'
    : 'var(--color-status-error)'
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
      <span class="badge" style="--_s: {statusVar};">
        <span class="dot"></span>
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
    font-size: 1.25rem; font-weight: 600; margin: 0 0 1rem; color: var(--color-text-primary);
  }
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
    background: rgba(0,0,0,0.08);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: rgba(0,0,0,0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--_s); }
  .version { color: var(--color-text-secondary); font-size: 0.85rem; }
  .metrics-dump {
    background: var(--color-surface); border: 1px solid var(--color-border); border-radius: 0.5rem;
    padding: 1rem; font-size: 0.8rem; color: var(--color-text-secondary);
    overflow: auto; max-height: 400px;
    font-family: 'JetBrains Mono', monospace;
  }
</style>
