<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';
  // #6360: the palace roster grew a delete action; the control itself is shared
  // with the Search tab so both confirm and report failures identically.
  import DeleteAction from './DeleteAction.svelte';
  // #6371: the non-destructive half of the roster's actions — reclaim a
  // palace's orphaned vectors without touching a drawer.
  import CompactAction from './CompactAction.svelte';
  // #6372: which of the three ways a row's counts were obtained decides how it
  // renders. The decision is a tested pure function, not template logic.
  import { countCell, sourceBadge, statsSource } from './palaceRows.js';

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);

  /**
   * Why: Fetches memory metrics while preventing concurrent in-flight requests
   *      from stacking (e.g. slow >20 s fetch overlapping the next interval tick
   *      or a rapid manual button click).
   * What: Returns early when a fetch is already in progress; otherwise sets the
   *       appropriate loading flag, fetches /api/console/metrics/memory, and
   *       stores the result or an error message.
   * Test: Call twice in rapid succession — assert only one HTTP request is made
   *       and state is consistent after both calls resolve.
   */
  async function fetchMetrics(isRefresh = false) {
    // Guard: drop the tick if a fetch is already in flight.
    // The very first call has refreshing=false and loading=true so it always
    // proceeds; subsequent interval ticks are dropped while busy.
    if (refreshing || (isRefresh && loading)) return;

    if (isRefresh) {
      refreshing = true;
    } else {
      loading = true;
    }
    try {
      const resp = await fetch('/api/console/metrics/memory');
      if (resp.status === 503) {
        error = 'trusty-memory metrics not yet available (daemon absent or first boot).';
        return;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      report = await resp.json();
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  /**
   * Re-read the palace roster from the daemon after a confirmed delete (#6360).
   *
   * Why not `fetchMetrics(true)`: that call drops itself when a background tick
   * is already in flight, and a dropped refresh right after a delete leaves the
   * deleted palace on screen until the next tick — which reads as a delete that
   * did not happen. This one always issues the request. The row is never
   * removed locally: what the daemon reports is what the table shows.
   */
  async function reloadRoster() {
    try {
      const resp = await fetch('/api/console/metrics/memory');
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      report = await resp.json();
      error = null;
    } catch (e) {
      error = e.message;
    }
  }

  /** Auto-refresh interval handle — cleared on component destroy to prevent leaks. */
  let refreshInterval;

  onMount(async () => {
    await fetchMetrics();
    refreshInterval = setInterval(() => fetchMetrics(true), 20_000);
  });

  onDestroy(() => {
    clearInterval(refreshInterval);
  });

  // Theme-adaptive CSS custom property ref (resolved against the active palette
  // at render time) instead of hardcoded hex — badge recolors on theme flip.
  let statusVar = $derived(
    report?.status === 'ok'       ? 'var(--trusty-success)'
    : report?.status === 'degraded' ? 'var(--trusty-warning)'
    : 'var(--trusty-danger)'
  );
</script>

<div class="tab-content">
  <RefreshHeader title="Trusty Memory" onRefresh={() => fetchMetrics(true)} {refreshing} />

  {#if loading}
    <div class="placeholder">Loading memory metrics…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else if report}
    <!-- Status badge + version -->
    <div class="meta-row">
      <span class="badge" style="--_s: {statusVar};">
        <span class="dot"></span>
        {report.status}
      </span>
      <span class="version">v{report.version}</span>
    </div>

    <!-- Aggregate stats -->
    <!--
      Why (#6372, amending #1924): the totals used to cover only palaces
      resident in trusty-memory's LRU cache, because #1924 stopped the daemon
      force-opening every palace on every poll. That left a host with 94
      palaces and 2 resident reporting two palaces' worth of drawers. The
      daemon now counts a closed palace off its files without opening it, so
      the headline is how many palaces were counted, and residency moves to the
      per-row badge where it belongs.
    -->
    <div class="stat-grid">
      <div class="stat-card">
        <span class="stat-value">
          {report.metrics?.counted_palace_count ?? report.metrics?.cached_palace_count ?? 0}<span class="stat-value-of">/{report.metrics?.palace_count ?? 0}</span>
        </span>
        <span class="stat-label">Palaces (counted/total)</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_drawers ?? 0}</span>
        <span class="stat-label">Total Drawers</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_vectors ?? 0}</span>
        <span class="stat-label">Total Vectors</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_rooms ?? 0}</span>
        <span class="stat-label">Total Rooms</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.total_kg_triples ?? 0}</span>
        <span class="stat-label">KG Triples</span>
      </div>
    </div>

    <!-- Per-palace table -->
    {#if report.metrics?.palaces?.length > 0}
      <h3 class="sub-title">Palaces (top {report.metrics.palaces.length})</h3>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Name</th>
              <th>Drawers</th>
              <th>Vectors</th>
              <th>Rooms</th>
              <th>KG Triples</th>
              <th class="actions-head">Actions</th>
            </tr>
          </thead>
          <tbody>
            {#each report.metrics.palaces as p (p.id)}
              <tr class:row-uncached={statsSource(p) === 'unavailable'}>
                <td><code>{p.id}</code></td>
                <td>
                  {p.name}
                  {#if sourceBadge(p)}
                    <span class="uncached-badge" title={sourceBadge(p).title}>{sourceBadge(p).label}</span>
                  {/if}
                </td>
                <td class="num">{countCell(p, 'drawer_count')}</td>
                <td class="num">{countCell(p, 'vector_count')}</td>
                <td class="num">{countCell(p, 'room_count')}</td>
                <td class="num">{countCell(p, 'kg_triple_count')}</td>
                <td class="actions">
                  <div class="row-actions">
                    <CompactAction id={p.id} onCompacted={reloadRoster} />
                    <DeleteAction kind="palace" id={p.id} onDeleted={reloadRoster} />
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="empty-hint">No palaces found.</p>
    {/if}
  {/if}
</div>

<style>
  .tab-content { padding: 0.25rem 0; }
  .placeholder, .not-available {
    background: var(--trusty-card-bg); border-radius: 0.5rem;
    padding: 1.25rem; color: var(--trusty-text-secondary); font-size: 0.9rem;
  }
  .not-available { color: var(--trusty-warning); }

  .meta-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem;
  }
  /* --_s supplied inline (statusVar) as a theme-adaptive --trusty-status-* ref. */
  .badge {
    display: inline-flex; align-items: center; gap: 0.35rem;
    font-size: 0.75rem; font-weight: 600; padding: 0.2rem 0.6rem;
    border-radius: 9999px; border: 1px solid;
    --_s: var(--trusty-text-muted);
    color: var(--_s);
    background: rgba(0,0,0,0.08);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: rgba(0,0,0,0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--_s); }
  .version { color: var(--trusty-text-secondary); font-size: 0.85rem; }

  .stat-grid {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; align-items: center; gap: 0.25rem;
  }
  .stat-value { font-size: 1.6rem; font-weight: 700; color: var(--trusty-text-primary); }
  .stat-value-of { font-size: 1rem; font-weight: 500; color: var(--trusty-text-secondary); }
  .stat-label { font-size: 0.75rem; color: var(--trusty-text-secondary); text-transform: uppercase; letter-spacing: 0.05em; }

  .sub-title { font-size: 1rem; font-weight: 600; color: var(--trusty-text-secondary); margin: 0 0 0.75rem; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  th {
    text-align: left; padding: 0.5rem 0.75rem;
    background: var(--trusty-card-bg); color: var(--trusty-text-secondary); font-weight: 600;
    border-bottom: 1px solid var(--trusty-border);
  }
  td { padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--trusty-card-bg); color: var(--trusty-text-primary); }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: var(--trusty-card-bg); }
  td.num { text-align: right; font-variant-numeric: tabular-nums; }
  /* #6360: the delete column. Right-aligned and vertically top-anchored so the
     expanded confirm panel grows downward without shifting the row's numbers. */
  th.actions-head, td.actions { text-align: right; }
  td.actions { vertical-align: top; }
  /* #6371: compact sits beside delete. They wrap rather than force the table
     wider, and each keeps its own confirm panel. */
  .row-actions { display: flex; gap: 0.35rem; justify-content: flex-end; flex-wrap: wrap; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    background: var(--trusty-surface-raised); padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }
  .empty-hint { color: var(--trusty-text-secondary); font-size: 0.85rem; }

  /* #6372: only a row whose counts could not be READ is greyed out. A palace
     that is merely closed carries real numbers now, so greying it would say
     the opposite of what it means. */
  tr.row-uncached td { color: var(--trusty-text-secondary); font-style: italic; }
  .uncached-badge {
    margin-left: 0.4rem; font-size: 0.68rem; font-style: normal; font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.04em;
    color: var(--trusty-text-secondary);
    background: var(--trusty-surface-raised);
    border-radius: 9999px; padding: 0.1rem 0.45rem;
  }
</style>
