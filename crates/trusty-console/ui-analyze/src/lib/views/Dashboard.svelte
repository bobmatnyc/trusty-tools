<script>
  /*
   * Why: Operator's at-a-glance health view — health pill, index picker
   * context, quality card (big letter grade + summary stats), and a top-10
   * complexity hotspot sidebar so the worst code is one click away.
   * What: Reads from centralized state; on index change refreshes quality
   * and hotspots. Renders status stats, quality grade card, hotspots list.
   * Test: Select an index in the topbar picker, observe the grade letter
   * and hotspot list re-render without page reload.
   */
  import {
    getHealth,
    getIndexes,
    getSelectedIndex,
    getQuality,
    getHotspots,
    refreshQuality,
    refreshHotspots
  } from '../state.svelte.js';
  import { navigate } from '../router.svelte.js';

  let health = $derived(getHealth());
  let indexes = $derived(getIndexes());
  let selected = $derived(getSelectedIndex());
  let quality = $derived(getQuality());
  let hotspots = $derived(getHotspots());

  // Auto-load quality + hotspots when the active index changes.
  $effect(() => {
    if (!selected) return;
    refreshQuality(selected).catch(() => {});
    refreshHotspots(selected, 10).catch(() => {});
  });

  let grade = $derived(quality?.grade || '?');
  let gradeClass = $derived('grade-' + (grade || '?').toString().toLowerCase());
  let top10 = $derived(hotspots.slice(0, 10));
</script>

<h1 class="page-title">Dashboard</h1>

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">Health</div>
    <div class="stat-value" style="font-size: 1.4rem; line-height: 1.4">
      {#if health?.status === 'ok'}
        <span class="badge badge-success">online</span>
      {:else}
        <span class="badge badge-danger">offline</span>
      {/if}
    </div>
    <div class="stat-meta">
      search {health?.search_reachable ? 'reachable' : 'unreachable'}
    </div>
  </div>
  <div class="stat">
    <div class="stat-label">Indexes</div>
    <div class="stat-value">{indexes.length}</div>
    <div class="stat-meta">corpora available</div>
  </div>
  <div class="stat">
    <div class="stat-label">Active</div>
    <div class="stat-value" style="font-size: 1.1rem">
      <span class="text-mono">{selected || '—'}</span>
    </div>
    <div class="stat-meta">selected index</div>
  </div>
  <div class="stat">
    <div class="stat-label">Smells</div>
    <div class="stat-value">{quality?.smell_count ?? 0}</div>
    <div class="stat-meta">detected</div>
  </div>
</div>

<div class="grid-2">
  <div class="card">
    <div class="card-header">Quality Grade</div>
    <div class="card-body">
      {#if indexes.length === 0}
        <div class="empty">
          No indexed projects found. Run <code class="text-mono">trusty-search index &lt;path&gt;</code>
          to index a project, then refresh.
        </div>
      {:else if !selected}
        <div class="empty">Select an index in the top bar to load quality metrics.</div>
      {:else if !quality}
        <div class="empty">Loading…</div>
      {:else}
        <div class="grade-row">
          <div class="grade-display {gradeClass}">{grade}</div>
          <div class="grade-meta">
            <div class="mini-stat">
              <div class="mini-label">Avg Cyclomatic</div>
              <div class="mini-value">{Number(quality.avg_cyclomatic ?? 0).toFixed(2)}</div>
            </div>
            <div class="mini-stat">
              <div class="mini-label">% Grade A</div>
              <div class="mini-value">
                {(Number(quality.pct_grade_a ?? 0) * 100).toFixed(1)}<span class="unit">%</span>
              </div>
            </div>
            <div class="mini-stat">
              <div class="mini-label">Smell Count</div>
              <div class="mini-value">{quality.smell_count ?? 0}</div>
            </div>
          </div>
        </div>
      {/if}
    </div>
  </div>

  <div class="card">
    <div class="card-header flex-between">
      <span>Top Complexity Hotspots</span>
      <button class="btn btn-sm btn-primary" onclick={() => navigate('/complexity')}>
        See all
      </button>
    </div>
    <div class="card-body" style="padding: 0">
      {#if indexes.length === 0}
        <div class="empty">
          No indexed projects. Run <code class="text-mono">trusty-search index &lt;path&gt;</code> first.
        </div>
      {:else if !selected}
        <div class="empty">No index selected.</div>
      {:else if top10.length === 0}
        <div class="empty">No hotspots reported.</div>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th>Function</th>
              <th>File</th>
              <th>Cyclo</th>
              <th>Grade</th>
            </tr>
          </thead>
          <tbody>
            {#each top10 as h}
              {@const m = h.metrics || {}}
              {@const g = (h.grade || m.grade || '?').toString()}
              <tr>
                <td class="text-mono text-xs">{h.function_name || h.symbol || '—'}</td>
                <td class="text-muted text-xs truncate" style="max-width: 280px">
                  {h.file || '—'}
                </td>
                <td><strong>{m.cyclomatic ?? h.cyclomatic ?? '—'}</strong></td>
                <td><span class="badge grade-{g.toLowerCase()}">{g}</span></td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .grid-2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--trusty-space-4);
  }
  @media (max-width: 1100px) {
    .grid-2 { grid-template-columns: 1fr; }
  }
  .grade-row {
    display: flex;
    gap: var(--trusty-space-5);
    align-items: center;
  }
  .grade-meta {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--trusty-space-3);
    flex: 1;
  }
  .mini-stat {
    padding: var(--trusty-space-3);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--trusty-radius);
  }
  .mini-label {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-bottom: 4px;
  }
  .mini-value {
    font-size: var(--trusty-fs-lg);
    font-weight: 700;
  }
  .unit {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    margin-left: 2px;
  }
</style>
