<script>
  /*
   * Why: Operators need a browser-only deep view of daemon resource usage —
   * RSS against its configured ceiling, CPU, disk footprint, uptime, and the
   * embedding-model detail. The TUI shows the headline numbers; this view is
   * the richer superset with gauge bars and the embedder block.
   * What: Auto-refreshing (5s) stat cards backed by `GET /health`, plus an
   * embedder section (model dimension, provider, quantized flag) and an
   * online/offline badge.
   * Test: `pnpm dev`, open #/health, confirm the RSS gauge fills relative to
   * rss_limit_mb and the badge turns green once /health responds.
   */
  import { onMount, onDestroy } from 'svelte';
  import { api } from '../api.js';

  let health = $state(null);
  let error = $state(null);
  let lastUpdated = $state(null);
  let timer = null;

  async function refresh() {
    try {
      health = await api.health();
      error = null;
      lastUpdated = new Date();
    } catch (e) {
      error = e.message || String(e);
      health = null;
    }
  }

  onMount(() => {
    refresh();
    timer = setInterval(refresh, 5000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  /**
   * Why: raw seconds are unreadable past a few minutes.
   * What: humanise to "Xs / Xm / Xh / Xd".
   * Test: humanUptime(7200) === "2h".
   */
  function humanUptime(secs) {
    if (typeof secs !== 'number' || secs < 0) return '—';
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h`;
    return `${Math.floor(h / 24)}d`;
  }

  /**
   * Why: disk_bytes is a raw byte count; operators want MB/GB.
   * What: human-readable byte size.
   * Test: humanBytes(1048576) === "1.0 MB".
   */
  function humanBytes(bytes) {
    if (typeof bytes !== 'number' || bytes < 0) return '—';
    if (bytes < 1024) return `${bytes} B`;
    const kb = bytes / 1024;
    if (kb < 1024) return `${kb.toFixed(1)} KB`;
    const mb = kb / 1024;
    if (mb < 1024) return `${mb.toFixed(1)} MB`;
    return `${(mb / 1024).toFixed(2)} GB`;
  }

  let rssPct = $derived.by(() => {
    const limit = health?.rss_limit_mb || 0;
    const used = health?.rss_mb || 0;
    if (limit <= 0) return null;
    return Math.min(100, (used / limit) * 100);
  });

  let online = $derived(!!health && health.status === 'ok');
</script>

<div class="page-head">
  <h1 class="page-title">Health</h1>
  <div class="head-meta">
    {#if online}
      <span class="badge badge-success">online</span>
    {:else}
      <span class="badge badge-danger">offline</span>
    {/if}
    {#if lastUpdated}
      <span class="text-xs text-muted">
        updated {lastUpdated.toLocaleTimeString()}
      </span>
    {/if}
  </div>
</div>

{#if error}
  <div class="card" style="border-color: var(--trusty-danger)">
    <div class="card-body" style="color: var(--trusty-danger)">{error}</div>
  </div>
{/if}

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">RSS Memory</div>
    <div class="stat-value">{(health?.rss_mb ?? 0).toLocaleString()} MB</div>
    {#if rssPct !== null}
      <div class="gauge mt-3">
        <div
          class="gauge-fill"
          class:gauge-warn={rssPct > 75}
          class:gauge-danger={rssPct > 90}
          style="width: {rssPct}%"
        ></div>
      </div>
      <div class="stat-meta">
        {rssPct.toFixed(0)}% of {health.rss_limit_mb.toLocaleString()} MB limit
      </div>
    {:else}
      <div class="stat-meta">no limit configured</div>
    {/if}
  </div>
  <div class="stat">
    <div class="stat-label">CPU</div>
    <div class="stat-value">{(health?.cpu_pct ?? 0).toFixed(1)}%</div>
    <div class="stat-meta">100% = one full core</div>
  </div>
  <div class="stat">
    <div class="stat-label">Disk</div>
    <div class="stat-value">{humanBytes(health?.disk_bytes)}</div>
    <div class="stat-meta">data directory footprint</div>
  </div>
  <div class="stat">
    <div class="stat-label">Uptime</div>
    <div class="stat-value">{humanUptime(health?.uptime_secs)}</div>
    <div class="stat-meta">daemon v{health?.version ?? '—'}</div>
  </div>
</div>

<div class="card mt-4">
  <div class="card-header">Embedder</div>
  <div class="card-body" style="padding: 0">
    <table class="table">
      <tbody>
        <tr>
          <th style="width: 240px">Status</th>
          <td>
            {#if health?.embedder === 'ready'}
              <span class="badge badge-success">ready</span>
            {:else if health?.embedder === 'initializing'}
              <span class="badge badge-warning">initializing</span>
            {:else if health?.embedder === 'error'}
              <span class="badge badge-danger">error</span>
            {:else}
              <span class="badge badge-muted">unavailable</span>
            {/if}
            {#if health?.embedder_error}
              <span class="text-sm" style="color: var(--trusty-danger)">
                {health.embedder_error}
              </span>
            {/if}
          </td>
        </tr>
        {#if health?.embedder_info}
          <tr>
            <th>Model</th>
            <td>
              all-MiniLM-L6-v2{health.embedder_info.quantized ? ' (INT8 quantized)' : ' (full precision)'}
            </td>
          </tr>
          <tr>
            <th>Vector dimension</th>
            <td class="text-mono">{health.embedder_info.dimension}</td>
          </tr>
          <tr>
            <th>ONNX provider</th>
            <td>
              <span class="badge badge-info">{health.embedder_info.provider}</span>
              {#if health.embedder_info.provider.startsWith('CoreML')}
                <span class="text-sm text-muted">Apple Silicon GPU / Neural Engine</span>
              {:else if health.embedder_info.provider === 'CUDA'}
                <span class="text-sm text-muted">NVIDIA GPU</span>
              {/if}
            </td>
          </tr>
        {:else}
          <tr>
            <th>Detail</th>
            <td class="text-muted text-sm">
              Embedder metadata not available yet — the daemon is warming up or
              running BM25-only.
            </td>
          </tr>
        {/if}
      </tbody>
    </table>
  </div>
</div>

<style>
  .page-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--trusty-space-5);
    flex-wrap: wrap;
    gap: var(--trusty-space-3);
  }
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0;
    font-weight: 600;
  }
  .head-meta {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
  }
  .gauge {
    width: 100%;
    height: 8px;
    border-radius: 999px;
    background: var(--trusty-border);
    overflow: hidden;
  }
  .gauge-fill {
    height: 100%;
    background: var(--trusty-success);
    transition: width 0.4s ease;
  }
  .gauge-fill.gauge-warn {
    background: var(--trusty-warning);
  }
  .gauge-fill.gauge-danger {
    background: var(--trusty-danger);
  }
  .table th {
    background: var(--trusty-content-bg);
    text-transform: none;
    letter-spacing: 0;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
</style>
