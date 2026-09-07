<script>
  /*
   * Why: Operators need an at-a-glance view of daemon resource usage (RSS,
   * disk, uptime) plus the dream-cycle health — a stalled background dream
   * loop is invisible without surfacing its last-run timestamp.
   * What: Auto-refreshing (5s) stat cards backed by `GET /health`,
   * `GET /api/v1/status`, and `GET /api/v1/dream/status`, with an
   * online/offline badge.
   * Test: open #/health, confirm the cards populate and the badge turns
   * green once /health responds.
   */
  import { onMount, onDestroy } from 'svelte';
  import { api } from '../api.js';

  let health = $state(null);
  let status = $state(null);
  let dream = $state(null);
  let error = $state(null);
  let lastUpdated = $state(null);
  let timer = null;

  async function refresh() {
    try {
      const [h, s, d] = await Promise.all([
        api.health(),
        api.status().catch(() => null),
        api.dreamStatus().catch(() => null)
      ]);
      health = h;
      status = s;
      dream = d;
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

  /**
   * Why: a dream's `last_run_at` is an ISO timestamp; operators want a
   * glanceable local time, or a clear "never" when no cycle has run.
   * What: localised date-time string, or "never".
   * Test: humanTime(null) === "never".
   */
  function humanTime(iso) {
    if (!iso) return 'never';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

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
      <span class="text-xs text-muted">updated {lastUpdated.toLocaleTimeString()}</span>
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
    <div class="stat-meta">resident set size</div>
  </div>
  <div class="stat">
    <div class="stat-label">Disk</div>
    <div class="stat-value">{humanBytes(health?.disk_bytes)}</div>
    <div class="stat-meta">data root footprint</div>
  </div>
  <div class="stat">
    <div class="stat-label">CPU</div>
    <div class="stat-value">{(health?.cpu_pct ?? 0).toFixed(1)}%</div>
    <div class="stat-meta">100% = one full core</div>
  </div>
  <div class="stat">
    <div class="stat-label">Uptime</div>
    <div class="stat-value">{humanUptime(health?.uptime_secs)}</div>
    <div class="stat-meta">daemon v{health?.version ?? '—'}</div>
  </div>
</div>

<div class="card mt-4">
  <div class="card-header">Dream cycle</div>
  <div class="card-body" style="padding: 0">
    <table class="table">
      <tbody>
        <tr>
          <th style="width: 240px">Last run</th>
          <td>{humanTime(dream?.last_run_at)}</td>
        </tr>
        <tr>
          <th>Merged</th>
          <td>{(dream?.merged ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Pruned</th>
          <td>{(dream?.pruned ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Compacted</th>
          <td>{(dream?.compacted ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Closets updated</th>
          <td>{(dream?.closets_updated ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Total duration</th>
          <td>{(dream?.duration_ms ?? 0).toLocaleString()} ms</td>
        </tr>
      </tbody>
    </table>
  </div>
</div>

<div class="card mt-4">
  <div class="card-header">Store totals</div>
  <div class="card-body" style="padding: 0">
    <table class="table">
      <tbody>
        <tr>
          <th style="width: 240px">Palaces</th>
          <td>{(status?.palace_count ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Drawers</th>
          <td>{(status?.total_drawers ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Vectors</th>
          <td>{(status?.total_vectors ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>KG triples</th>
          <td>{(status?.total_kg_triples ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Data root</th>
          <td class="text-mono text-xs text-muted">{status?.data_root ?? '—'}</td>
        </tr>
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
  .table th {
    background: var(--trusty-content-bg);
    text-transform: none;
    letter-spacing: 0;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
</style>
