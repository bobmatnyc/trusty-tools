<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';
  // #6923: this tab displays; the search dashboard manages (DOC-73 §13). The
  // inline delete (#6360) and the stale-registration cleanup panel (#6371) are
  // gone, and a row now opens the index's management view instead (§14).
  import {
    indexDashboardHref,
    indexRowAriaLabel,
    indexRowHint,
  } from './searchIndexNav.js';
  // #6424: the Last Used column and its sort. Shared with the Memory tab so
  // both rosters agree on what a missing timestamp means.
  import {
    formatLastUsed,
    lastUsedTitle,
    nextSortDirection,
    sortByLastUsed,
    sortIndicator,
  } from './lastUsed.js';

  /** Format bytes into a human-readable string (KB / MB / GB). */
  function formatBytes(bytes) {
    if (bytes == null) return '—';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);

  /**
   * Why: Fetches search metrics while preventing concurrent in-flight requests
   *      from stacking (e.g. slow >20 s fetch overlapping the next interval tick
   *      or a rapid manual button click).
   * What: Returns early when a fetch is already in progress; otherwise sets the
   *       appropriate loading flag, fetches /api/console/metrics/search, and
   *       stores the result or an error message.
   * Test: Call twice in rapid succession — assert only one HTTP request is made
   *       and state is consistent after both calls resolve.
   */
  async function fetchMetrics(isRefresh = false) {
    // Guard: drop the tick if a fetch is already in flight.
    if (refreshing || (isRefresh && loading)) return;

    if (isRefresh) {
      refreshing = true;
    } else {
      loading = true;
    }
    try {
      const resp = await fetch('/api/console/metrics/search');
      if (resp.status === 503) {
        error = 'trusty-search metrics not yet available (daemon absent or first boot).';
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

  /** Auto-refresh interval handle — cleared on component destroy to prevent leaks. */
  let refreshInterval;

  onMount(async () => {
    await fetchMetrics();
    refreshInterval = setInterval(() => fetchMetrics(true), 20_000);
  });

  onDestroy(() => {
    clearInterval(refreshInterval);
  });

  // Theme-adaptive CSS custom property refs (resolved against the active palette
  // at render time) instead of hardcoded hex — badges/stats recolor on theme flip.
  let statusVar = $derived(
    report?.status === 'ok'         ? 'var(--trusty-success)'
    : report?.status === 'degraded' ? 'var(--trusty-warning)'
    : 'var(--trusty-danger)'
  );

  // #6424: `null` is the daemon's own order — the only way back once a sort has
  // been applied, which is why the header cycles through it.
  let lastUsedSort = $state(null);
  let indexRows = $derived(
    lastUsedSort
      ? sortByLastUsed(report?.metrics?.indexes ?? [], lastUsedSort)
      : (report?.metrics?.indexes ?? [])
  );
</script>

<div class="tab-content">
  <RefreshHeader title="Trusty Search" onRefresh={() => fetchMetrics(true)} {refreshing} />

  <!-- #6155: the console now serves the full trusty-search dashboard. This card
       stays a status summary; the link is how anyone reaches search, browse and
       per-index configuration. -->
  <p class="dashboard-link">
    <a href="/tools/search/">Open the Trusty Search dashboard &rarr;</a>
  </p>

  {#if loading}
    <div class="placeholder">Loading search metrics…</div>
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
    <div class="stat-grid">
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.index_count ?? 0}</span>
        <span class="stat-label">Indexes</span>
      </div>
      <!-- #6923: the card used to be labelled "Warm Boot Degraded", which named
           the field rather than what it measures. The flag it reads
           (`warmboot_summary.warm_boot_degraded`, recomputed in trusty-search's
           `health.rs:1040`) is true when ANY index is not fully serving: a
           failed embed stage, a TCC or allowlist skip at boot, a load timeout,
           or a registry that came back smaller than it was. The title says
           which, since the card itself is one word. -->
      <div
        class="stat-card"
        class:degraded={report.metrics?.warm_boot_degraded}
        title="Some index is not fully serving: a failed embed stage, a boot-time TCC or allowlist skip, a load timeout, or a registry smaller than it was."
      >
        <span class="stat-value">
          {report.metrics?.warm_boot_degraded ? 'Yes' : 'No'}
        </span>
        <span class="stat-label">Indexes Degraded</span>
      </div>
    </div>

    <!-- #6923: the roster is a grid list, not a `<table>`, for the reason
         `ServicesList.svelte` gives: a row that navigates must BE the link, and
         a link cannot wrap a `<tr>`. The Actions column is gone with the inline
         delete — the row itself is now the one control it carries. -->
    {#if report.metrics?.indexes?.length > 0}
      <h3 class="sub-title">Indexes</h3>
      <div class="list">
        <div class="row head">
          <span aria-hidden="true">ID</span>
          <span aria-hidden="true">Root Path</span>
          <span class="num" aria-hidden="true">Size</span>
          <!-- #6424: click to cycle newest-first, oldest-first, daemon order. -->
          <span class="num sortable">
            <button
              type="button"
              class="sort-btn"
              aria-label="Sort by last used"
              onclick={() => (lastUsedSort = nextSortDirection(lastUsedSort))}
            >
              Last Used <span class="sort-arrow">{sortIndicator(lastUsedSort)}</span>
            </button>
          </span>
        </div>

        {#snippet indexCells(idx, inertHint)}
          <span class="mono">
            {idx.id ?? '—'}
            {#if inertHint}<span class="sr-only">— {inertHint}</span>{/if}
          </span>
          <span class="path">{idx.root_path ?? '—'}</span>
          <span class="num">
            {idx.size_bytes != null ? formatBytes(idx.size_bytes) : '—'}
          </span>
          <span class="num" title={lastUsedTitle(idx)}>{formatLastUsed(idx)}</span>
        {/snippet}

        {#each indexRows as idx (idx.id)}
          {#if idx.id}
            <a
              class="row link"
              href={indexDashboardHref(idx.id)}
              aria-label={indexRowAriaLabel({
                id: idx.id,
                rootPath: idx.root_path ?? 'no root path',
                size: idx.size_bytes != null ? formatBytes(idx.size_bytes) : 'size unknown',
                lastUsed: formatLastUsed(idx),
              })}
            >
              {@render indexCells(idx, null)}
            </a>
          {:else}
            <!-- A registration with no id has no management view to open, so
                 the row is inert and says why — on `title` for a pointer AND in
                 a visually hidden span for a screen reader, which is the whole
                 shape an undashboarded service row uses in
                 `ServicesList.svelte`, not just its `title` half. -->
            <div class="row inert" title={indexRowHint(idx)}>
              {@render indexCells(idx, indexRowHint(idx))}
            </div>
          {/if}
        {/each}
      </div>
    {:else}
      <p class="empty-hint">No indexes registered.</p>
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
    display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; align-items: center; gap: 0.25rem;
  }
  /* #6923: the card centres its own text. `align-items: center` above centres
     each line as a flex ITEM, which is not the same thing once a label wraps to
     two lines — the second line was left-aligned under the first. */
  .stat-value {
    font-size: 1.6rem; font-weight: 700; color: var(--trusty-text-primary);
    text-align: center;
  }
  .stat-label {
    font-size: 0.75rem; color: var(--trusty-text-secondary);
    text-transform: uppercase; letter-spacing: 0.05em;
    text-align: center;
  }
  /* .degraded modifier: warm-boot-degraded card gets amber border/value tint,
     consistent with the --_s badge pattern (single scoped var, no inline one-offs). */
  .stat-card.degraded {
    --_s: var(--trusty-warning);
    border-color: rgba(0,0,0,0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .stat-card.degraded .stat-value { color: var(--trusty-warning); }

  .sub-title { font-size: 1rem; font-weight: 600; color: var(--trusty-text-secondary); margin: 0 0 0.75rem; }

  /* #6923: the roster list, built the way `ServicesList.svelte` builds its own
     — one grid row per index, the row itself the link. Same tokens, same
     borders, same hover and focus treatment; nothing new is invented here. */
  .list {
    background: var(--trusty-card-bg);
    border: 1.5px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    overflow: hidden;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(6rem, 0.8fr) minmax(10rem, 2fr) 6rem 7rem;
    align-items: center;
    gap: var(--trusty-space-4);
    width: 100%;
    padding: 0.5rem var(--trusty-space-5);
    border-bottom: 1px solid var(--trusty-surface-raised);
    font-size: var(--trusty-fs-sm);
    text-align: left;
    color: var(--trusty-text-primary);
    text-decoration: none;
  }
  .row:last-child { border-bottom: none; }
  .row.head {
    font: 600 10px var(--trusty-mono);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--trusty-text-muted);
    border-bottom: 1px solid var(--trusty-border);
  }
  .row.link:hover { background: var(--trusty-surface-hover); }
  .row.link:focus-visible {
    outline: 2px solid var(--trusty-accent);
    outline-offset: -2px;
  }
  /* No management view for this row → no affordance. */
  .row.inert { cursor: default; color: var(--trusty-text-muted); }

  .mono { font-family: var(--trusty-mono); font-size: var(--trusty-fs-xs); }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .path {
    font-size: 0.8rem; color: var(--trusty-text-secondary);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  /* #6424: the sortable header is a real button so it is keyboard-reachable;
     it inherits the header's type so only the arrow marks it as interactive. */
  .row.head .sortable { padding: 0; }
  .sort-btn {
    width: 100%; background: none; border: none; cursor: pointer;
    font: inherit; color: inherit; text-align: right; padding: 0;
  }
  .sort-btn:hover { color: var(--trusty-text-primary); }
  .sort-arrow { opacity: 0.6; margin-left: 0.2rem; }

  /* The same visually-hidden stamp `ServicesList.svelte` uses, so an inert row
     reads its reason aloud rather than only on hover. */
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  .empty-hint { color: var(--trusty-text-secondary); font-size: 0.85rem; }
  .dashboard-link { margin: 0 0 1rem; font-size: 0.85rem; }
  .dashboard-link a { color: var(--trusty-accent); text-decoration: none; }
  .dashboard-link a:hover { text-decoration: underline; }
</style>
