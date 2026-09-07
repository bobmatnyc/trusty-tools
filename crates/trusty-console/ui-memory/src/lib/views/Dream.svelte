<script>
  /*
   * Why: The dream cycle is trusty-memory's background memory-maintenance
   * pass (merge near-duplicates, prune stale drawers, compact closets).
   * Operators want to trigger it on demand after a bulk ingest and see the
   * resulting counts.
   * What: A "Trigger Dream Cycle" button → `POST /api/v1/dream/run`, the
   * latest persisted aggregate from `GET /api/v1/dream/status`, and a
   * session-local history table of runs triggered from this browser tab.
   * Test: open #/dream, click Trigger, confirm a result row appears with
   * merged / pruned / compacted counts.
   */
  import { onMount } from 'svelte';
  import { api } from '../api.js';

  let lastStatus = $state(null);
  let statusError = $state(null);
  let running = $state(false);
  let runError = $state(null);
  // Session-local history: each entry is { at, merged, pruned, compacted,
  // closets_updated, duration_ms }. The daemon does not expose a run-history
  // endpoint, so this records runs triggered from this tab.
  let history = $state([]);

  onMount(loadStatus);

  async function loadStatus() {
    try {
      lastStatus = await api.dreamStatus();
      statusError = null;
    } catch (e) {
      statusError = e.message || String(e);
    }
  }

  /**
   * Why: a dream cycle can take noticeable time on large palaces; we disable
   * the button and surface the aggregate result when it returns.
   * What: POSTs `/api/v1/dream/run`, prepends the result to the session
   * history, and refreshes the persisted status snapshot.
   * Test: click Trigger, confirm the history table gains a row.
   */
  async function triggerDream() {
    running = true;
    runError = null;
    try {
      const stats = await api.dreamRun();
      history = [
        {
          at: new Date(),
          merged: stats.merged ?? 0,
          pruned: stats.pruned ?? 0,
          compacted: stats.compacted ?? 0,
          closets_updated: stats.closets_updated ?? 0,
          duration_ms: stats.duration_ms ?? 0
        },
        ...history
      ];
      lastStatus = stats;
    } catch (e) {
      runError = e.message || String(e);
    } finally {
      running = false;
    }
  }

  /**
   * Why: ISO timestamps are precise but not glanceable.
   * What: localised date-time, or "never" when null.
   * Test: humanTime(null) === "never".
   */
  function humanTime(value) {
    if (!value) return 'never';
    const d = value instanceof Date ? value : new Date(value);
    if (Number.isNaN(d.getTime())) return String(value);
    return d.toLocaleString();
  }
</script>

<h1 class="page-title">Dream</h1>

<div class="card mb-4">
  <div class="card-body dream-action">
    <div>
      <div class="text-sm text-secondary">
        Run a dream cycle across every palace — merges near-duplicate drawers,
        prunes stale memories, and compacts closets.
      </div>
      <div class="text-xs text-muted mt-3">
        Endpoint: <code>POST /api/v1/dream/run</code>
      </div>
    </div>
    <button class="btn btn-primary" onclick={triggerDream} disabled={running}>
      {running ? 'Dreaming…' : 'Trigger Dream Cycle'}
    </button>
  </div>
  {#if runError}
    <div class="dream-error">{runError}</div>
  {/if}
</div>

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">Merged</div>
    <div class="stat-value">{(lastStatus?.merged ?? 0).toLocaleString()}</div>
    <div class="stat-meta">duplicate drawers</div>
  </div>
  <div class="stat">
    <div class="stat-label">Pruned</div>
    <div class="stat-value">{(lastStatus?.pruned ?? 0).toLocaleString()}</div>
    <div class="stat-meta">stale drawers</div>
  </div>
  <div class="stat">
    <div class="stat-label">Compacted</div>
    <div class="stat-value">{(lastStatus?.compacted ?? 0).toLocaleString()}</div>
    <div class="stat-meta">closets</div>
  </div>
  <div class="stat">
    <div class="stat-label">Last run</div>
    <div class="stat-value" style="font-size: var(--trusty-fs-md)">
      {humanTime(lastStatus?.last_run_at)}
    </div>
    <div class="stat-meta">persisted aggregate</div>
  </div>
</div>

<div class="card">
  <div class="card-header">Run history (this session)</div>
  <div class="card-body" style="padding: 0">
    {#if statusError}
      <div class="dream-error">{statusError}</div>
    {/if}
    {#if history.length === 0}
      <div class="empty">
        No dream cycles triggered from this tab yet. The daemon does not expose
        a persisted run-history endpoint — runs you trigger here are listed
        below.
      </div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Triggered at</th>
            <th>Merged</th>
            <th>Pruned</th>
            <th>Compacted</th>
            <th>Closets</th>
            <th>Duration</th>
          </tr>
        </thead>
        <tbody>
          {#each history as run, i (i)}
            <tr>
              <td class="text-sm">{humanTime(run.at)}</td>
              <td>{run.merged.toLocaleString()}</td>
              <td>{run.pruned.toLocaleString()}</td>
              <td>{run.compacted.toLocaleString()}</td>
              <td>{run.closets_updated.toLocaleString()}</td>
              <td>{run.duration_ms.toLocaleString()} ms</td>
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
  .dream-action {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--trusty-space-4);
    flex-wrap: wrap;
  }
  .text-secondary {
    color: var(--trusty-text-secondary);
  }
  .dream-error {
    padding: var(--trusty-space-3) var(--trusty-space-5);
    color: var(--trusty-danger);
    background: var(--trusty-danger-soft);
    font-size: var(--trusty-fs-sm);
  }
</style>
