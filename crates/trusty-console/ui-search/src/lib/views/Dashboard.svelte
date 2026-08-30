<script>
  /*
   * Why: The dashboard is the operator's at-a-glance view of daemon health
   * and the index catalogue — it surfaces the four headline numbers
   * (Indexes, Total documents, Uptime, Version) plus a recent-indexes table
   * that doubles as a jump-off to the Indexes view.
   * What: Stat-cards grid + a recent-indexes table. All data flows through
   * the centralised state store (`state.svelte.js`).
   * Test: `pnpm dev` in `ui/`, open http://127.0.0.1:7878/ui, confirm the
   * counters render and clicking a row navigates to /indexes.
   */
  import { onMount, onDestroy } from 'svelte';
  import {
    getHealth,
    getIndexes,
    getLiveStats,
    subscribeStatusStream,
    unsubscribeStatusStream
  } from '../state.svelte.js';
  import { navigate } from '../router.svelte.js';

  let health = $derived(getHealth());
  let indexes = $derived(getIndexes());
  let liveStats = $derived(getLiveStats());

  // Prefer the live stream value (covers indexes the dashboard's per-index
  // /status fetch hasn't refreshed yet); fall back to the locally computed
  // sum so the first paint isn't blank.
  let totalDocuments = $derived(
    liveStats?.total_chunks ??
      indexes.reduce((sum, ix) => sum + (ix.chunk_count || 0), 0)
  );

  // Why: open one EventSource per mount, close on unmount so we never leak
  // connections when the user navigates between views.
  onMount(() => {
    subscribeStatusStream();
  });
  onDestroy(() => {
    unsubscribeStatusStream();
  });

  let recent = $derived(
    [...indexes]
      .sort((a, b) => (b.chunk_count || 0) - (a.chunk_count || 0))
      .slice(0, 10)
  );

  /**
   * Why: Operators want a quick "how long has this been up?" signal — raw
   * second counts get unreadable past a few minutes.
   * What: Returns a humanised "Xs / Xm / Xh / Xd" string.
   * Test: Pass 7200 (2 hours), expect "2h".
   */
  function humanUptime(secs) {
    if (typeof secs !== 'number' || secs < 0) return '—';
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h`;
    const d = Math.floor(h / 24);
    return `${d}d`;
  }
</script>

<h1 class="page-title">Dashboard</h1>

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">Indexes</div>
    <div class="stat-value">{indexes.length}</div>
    <div class="stat-meta">registered</div>
  </div>
  <div class="stat">
    <div class="stat-label">Documents</div>
    <div class="stat-value">{totalDocuments.toLocaleString()}</div>
    <div class="stat-meta">indexed chunks</div>
  </div>
  <div class="stat">
    <div class="stat-label">Uptime</div>
    <div class="stat-value">{humanUptime(health?.uptime_secs)}</div>
    <div class="stat-meta">daemon</div>
  </div>
  <div class="stat">
    <div class="stat-label">Version</div>
    <div class="stat-value text-mono" style="font-size: var(--trusty-fs-lg)">
      {health?.version ?? '—'}
    </div>
    <div class="stat-meta">
      {#if health?.status === 'ok'}
        <span class="badge badge-success">healthy</span>
      {:else}
        <span class="badge badge-muted">offline</span>
      {/if}
    </div>
  </div>
</div>

<div class="card mt-4">
  <div class="card-header flex-between">
    <span>Recent indexes</span>
    <button class="btn btn-sm btn-primary" onclick={() => navigate('/indexes')}>
      Manage all
    </button>
  </div>
  <div class="card-body" style="padding: 0">
    {#if recent.length === 0}
      <div class="empty">
        No indexes yet.
        <a
          href="#/indexes"
          onclick={(e) => {
            e.preventDefault();
            navigate('/indexes');
          }}>Create one</a
        >.
      </div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Documents</th>
            <th>Root path</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {#each recent as ix}
            <tr
              style="cursor: pointer"
              onclick={() => navigate('/indexes')}
            >
              <td><strong>{ix.id}</strong></td>
              <td>{(ix.chunk_count ?? 0).toLocaleString()}</td>
              <td class="text-mono text-xs text-muted truncate" style="max-width: 360px">
                {ix.root_path || '—'}
              </td>
              <td>
                {#if ix.error}
                  <span class="badge badge-danger">error</span>
                {:else}
                  <span class="badge badge-success">ready</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
</style>
