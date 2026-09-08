<script>
  /*
   * Why (#6928): the owner's ruling makes this tab display-only — "memory
   * console should show disk and ram usage, actions should be moved to the
   * dashboard". It used to carry an inline compact and delete per palace
   * (#6371, #6360); both now live on `/tools/memory`, and a row opens that
   * palace's view there instead. Same shape #6923 gave the Search tab.
   * What: two usage panels — the palace store's disk footprint and the
   * daemon's physical footprint with its heap / file-backed / compressed split
   * (#7084) — over a roster whose rows are links, each carrying that palace's
   * own disk size.
   * Test: `memoryTabDisplayOnly.test.js` pins the absence of every mutating
   * surface; `memoryUsage.test.js` and `palaceNav.test.js` cover what it shows.
   */
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';
  // #6928: the two figures this tab exists to display, and where a row goes.
  import { diskUsage, palaceDiskCell, ramUsage } from './memoryUsage.js';
  import {
    palaceDashboardHref,
    palaceRowAriaLabel,
    palaceRowHint,
  } from './palaceNav.js';
  // #6372: which of the three ways a row's counts were obtained decides how it
  // renders. The decision is a tested pure function, not template logic.
  import { countCell, sourceBadge, statsSource } from './palaceRows.js';
  // #6424: the Last Used column and its sort. Shared with the Search tab so
  // both rosters agree on what a missing timestamp means.
  import {
    formatLastUsed,
    lastUsedTitle,
    nextSortDirection,
    sortByLastUsed,
    sortIndicator,
  } from './lastUsed.js';

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

  let ram = $derived(ramUsage(report?.metrics));
  let disk = $derived(diskUsage(report?.metrics));

  // #6424: `null` is the daemon's own order — the only way back once a sort has
  // been applied, which is why the header cycles through it.
  let lastUsedSort = $state(null);
  let sortedPalaces = $derived(
    lastUsedSort
      ? sortByLastUsed(report?.metrics?.palaces ?? [], lastUsedSort)
      : (report?.metrics?.palaces ?? [])
  );
</script>

<div class="tab-content">
  <RefreshHeader title="Trusty Memory" onRefresh={() => fetchMetrics(true)} {refreshing} />

  <!-- #6928: every action this tab used to carry — and dream, stop, compact,
       re-embed and delete besides — is on the dashboard. This link is how an
       operator reaches all of them. -->
  <p class="dashboard-link">
    <a href="/tools/memory/">Open the Trusty Memory dashboard &rarr;</a>
  </p>

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

    <!-- #6928: the two usage panels. Disk first — it is the figure that grows
         without anyone watching; RAM second, with the split that says whether a
         large footprint is a heap or a mapped store. -->
    <div class="usage-row">
      <section class="usage-card">
        <h3 class="usage-title">Disk</h3>
        <p class="usage-figure">{disk.text}</p>
        <p class="usage-meta">
          on-disk size of the palace store
          {#if disk.dataRoot}
            <br /><code class="path">{disk.dataRoot}</code>
          {/if}
        </p>
      </section>

      <section class="usage-card">
        <h3 class="usage-title">RAM</h3>
        <p class="usage-figure">{ram.footprint}</p>
        <p class="usage-meta">physical footprint of the daemon process</p>
        {#if ram.reported}
          <dl class="breakdown">
            {#each ram.components as c (c.label)}
              <div class="breakdown-row">
                <dt>{c.label}</dt>
                <dd>{c.text}</dd>
              </div>
            {/each}
          </dl>
        {:else}
          <p class="usage-note">{ram.note}</p>
        {/if}
      </section>
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

    <!-- #6928: the roster is a grid list, not a `<table>`, for the reason
         `SearchTab.svelte` and `ServicesList.svelte` both give: a row that
         navigates must BE the link, and a link cannot wrap a `<tr>`. The
         Actions column is gone with the inline compact and delete — the row
         itself is now the one control it carries. -->
    {#if report.metrics?.palaces?.length > 0}
      <h3 class="sub-title">Palaces (top {report.metrics.palaces.length})</h3>
      <div class="list">
        <div class="row head">
          <span aria-hidden="true">ID</span>
          <span aria-hidden="true">Name</span>
          <span class="num" aria-hidden="true">Drawers</span>
          <span class="num" aria-hidden="true">Vectors</span>
          <span class="num" aria-hidden="true">Rooms</span>
          <span class="num" aria-hidden="true">KG</span>
          <span class="num" aria-hidden="true">Disk</span>
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

        {#snippet palaceCells(p, inertHint)}
          <span class="mono">
            {p.id ?? '—'}
            {#if inertHint}<span class="sr-only">— {inertHint}</span>{/if}
          </span>
          <span class="name">
            {p.name}
            {#if sourceBadge(p)}
              <span class="uncached-badge" title={sourceBadge(p).title}>{sourceBadge(p).label}</span>
            {/if}
          </span>
          <span class="num">{countCell(p, 'drawer_count')}</span>
          <span class="num">{countCell(p, 'vector_count')}</span>
          <span class="num">{countCell(p, 'room_count')}</span>
          <span class="num">{countCell(p, 'kg_triple_count')}</span>
          <span class="num">{palaceDiskCell(p)}</span>
          <span class="num" title={lastUsedTitle(p)}>{formatLastUsed(p)}</span>
        {/snippet}

        {#each sortedPalaces as p (p.id)}
          {#if p.id}
            <a
              class="row link"
              class:row-uncached={statsSource(p) === 'unavailable'}
              href={palaceDashboardHref(p.id)}
              aria-label={palaceRowAriaLabel({
                name: p.name ?? 'unnamed',
                id: p.id,
                drawers: countCell(p, 'drawer_count'),
                disk: palaceDiskCell(p),
                lastUsed: formatLastUsed(p),
              })}
            >
              {@render palaceCells(p, null)}
            </a>
          {:else}
            <!-- A palace with no id has no management view to open, so the row
                 is inert and says why — on `title` for a pointer AND in a
                 visually hidden span for a screen reader, the shape an
                 undashboarded service row uses in `ServicesList.svelte`. -->
            <div class="row inert" title={palaceRowHint(p)}>
              {@render palaceCells(p, palaceRowHint(p))}
            </div>
          {/if}
        {/each}
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

  .dashboard-link { margin: 0 0 1rem; font-size: 0.9rem; }
  .dashboard-link a { color: var(--trusty-accent); text-decoration: none; }
  .dashboard-link a:hover { text-decoration: underline; }

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

  /* #6928: the two usage panels. Same card tokens as .stat-card, wider so the
     RAM breakdown has room for three labelled rows. */
  .usage-row {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .usage-card {
    background: var(--trusty-card-bg); border: 1px solid var(--trusty-border);
    border-radius: 0.5rem; padding: 1rem;
  }
  .usage-title {
    margin: 0 0 0.35rem; font-size: 0.75rem; font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.05em;
    color: var(--trusty-text-secondary);
  }
  .usage-figure {
    margin: 0; font-size: 1.8rem; font-weight: 700;
    color: var(--trusty-text-primary); font-variant-numeric: tabular-nums;
  }
  .usage-meta {
    margin: 0.25rem 0 0; font-size: 0.78rem; color: var(--trusty-text-secondary);
  }
  .usage-note {
    margin: 0.6rem 0 0; font-size: 0.78rem; color: var(--trusty-text-secondary);
    font-style: italic;
  }
  .path { font-size: 0.72rem; word-break: break-all; }
  .breakdown { margin: 0.7rem 0 0; }
  .breakdown-row {
    display: flex; justify-content: space-between; gap: 1rem;
    font-size: 0.82rem; padding: 0.2rem 0;
    border-top: 1px solid var(--trusty-border);
  }
  .breakdown-row dt { color: var(--trusty-text-secondary); }
  .breakdown-row dd {
    margin: 0; color: var(--trusty-text-primary);
    font-variant-numeric: tabular-nums;
  }

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
  .stat-label {
    font-size: 0.75rem; color: var(--trusty-text-secondary);
    text-transform: uppercase; letter-spacing: 0.05em; text-align: center;
  }

  .sub-title { font-size: 1rem; font-weight: 600; color: var(--trusty-text-secondary); margin: 0 0 0.75rem; }

  /* #6928: the roster list, built the way `SearchTab.svelte` builds its own —
     one grid row per palace, the row itself the link. Same tokens, same
     borders, same hover and focus treatment. */
  .list {
    background: var(--trusty-card-bg);
    border: 1.5px solid var(--trusty-border);
    border-radius: var(--trusty-radius, 0.5rem);
    overflow-x: auto;
  }
  .row {
    display: grid;
    grid-template-columns:
      minmax(6rem, 1fr) minmax(8rem, 1.4fr)
      5rem 5rem 4.5rem 5rem 6rem 7rem;
    align-items: center;
    gap: 0.75rem;
    width: 100%;
    padding: 0.5rem 0.75rem;
    font-size: 0.85rem;
    border-bottom: 1px solid var(--trusty-border);
    color: var(--trusty-text-primary);
    text-decoration: none;
    box-sizing: border-box;
  }
  .row:last-child { border-bottom: none; }
  .row.head {
    background: var(--trusty-surface-raised, var(--trusty-card-bg));
    color: var(--trusty-text-secondary);
    font-weight: 600;
  }
  .row.link:hover { background: var(--trusty-surface-raised, var(--trusty-card-bg)); }
  .row.link:focus-visible { outline: 2px solid var(--trusty-accent); outline-offset: -2px; }
  .row.inert { color: var(--trusty-text-secondary); }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .mono {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* #6424: the sortable header is a real button so it is keyboard-reachable;
     it inherits the header's type so only the arrow marks it as interactive. */
  .sortable { padding: 0; }
  .sort-btn {
    width: 100%; background: none; border: none; cursor: pointer;
    font: inherit; color: inherit; text-align: right; padding: 0;
  }
  .sort-btn:hover { color: var(--trusty-text-primary); }
  .sort-arrow { opacity: 0.6; margin-left: 0.2rem; }

  .empty-hint { color: var(--trusty-text-secondary); font-size: 0.85rem; }

  /* #6372: only a row whose counts could not be READ is greyed out. A palace
     that is merely closed carries real numbers now, so greying it would say
     the opposite of what it means. */
  .row-uncached { color: var(--trusty-text-secondary); font-style: italic; }
  .uncached-badge {
    margin-left: 0.4rem; font-size: 0.68rem; font-style: normal; font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.04em;
    color: var(--trusty-text-secondary);
    background: var(--trusty-surface-raised);
    border-radius: 9999px; padding: 0.1rem 0.45rem;
  }

  /* Visually hidden but read aloud — the shape `ServicesList.svelte` uses. */
  .sr-only {
    position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
    overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0;
  }
</style>
