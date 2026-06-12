<script>
  import { onMount } from 'svelte';

  // ─── state ──────────────────────────────────────────────────────────────────

  let report = $state(null);
  let metricsLoading = $state(true);
  let metricsError = $state(null);

  let indexes = $state([]);
  let indexesLoading = $state(false);
  let indexesError = $state(null);

  let selectedIndex = $state(null);
  let entities = $state([]);
  let clusters = $state([]);
  let vizLoading = $state(false);
  let vizError = $state(null);

  // ─── fetch on mount ─────────────────────────────────────────────────────────

  onMount(async () => {
    await Promise.all([fetchMetrics(), fetchIndexes()]);
  });

  async function fetchMetrics() {
    try {
      const resp = await fetch('/api/console/metrics/analyze');
      if (resp.status === 503) {
        metricsError = 'trusty-analyze metrics not yet available (daemon absent or first boot).';
        return;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      report = await resp.json();
    } catch (e) {
      metricsError = e.message;
    } finally {
      metricsLoading = false;
    }
  }

  async function fetchIndexes() {
    indexesLoading = true;
    try {
      // Use the console's own API route — the console calls trusty-analyze over
      // stdio MCP internally. The browser never contacts the analyze daemon HTTP
      // directly (architecture: console is a stdio MCP client only, per #1104).
      const resp = await fetch('/api/console/metrics/analyze/indexes');
      if (resp.status === 503) {
        // Analyze binary absent or in backoff — non-fatal.
        indexesError = 'trusty-analyze not available (binary absent or starting up).';
        return;
      }
      if (!resp.ok) {
        indexesError = `Index list error: HTTP ${resp.status}`;
        return;
      }
      const data = await resp.json();
      // The analyze daemon returns [{id: "..."}, ...] — handle both array and
      // direct value.
      indexes = Array.isArray(data) ? data : (data?.indexes ?? []);
      // Auto-select first index for the visualizer.
      if (indexes.length > 0) {
        selectedIndex = indexes[0].id;
        await loadViz(selectedIndex);
      }
    } catch (e) {
      indexesError = e.message;
    } finally {
      indexesLoading = false;
    }
  }

  async function loadViz(indexId) {
    if (!indexId) return;
    vizLoading = true;
    vizError = null;
    entities = [];
    clusters = [];
    try {
      // Use the console's own API route — the console calls trusty-analyze over
      // stdio MCP internally. No /proxy/analyze usage from the browser (#1104).
      const resp = await fetch(
        `/api/console/metrics/analyze/visualize?index=${encodeURIComponent(indexId)}`
      );
      if (resp.status === 503) {
        vizError = 'trusty-analyze not available (binary absent or starting up).';
        return;
      }
      if (!resp.ok) {
        vizError = `Failed to load visualization data: HTTP ${resp.status}`;
        return;
      }
      const data = await resp.json();
      if (data.error) {
        vizError = data.error;
        return;
      }
      // Combined response: { graph, entities, clusters }
      entities = Array.isArray(data.entities) ? data.entities : [];
      clusters = data.clusters ?? null;
    } catch (e) {
      vizError = e.message;
    } finally {
      vizLoading = false;
    }
  }

  async function onIndexChange(e) {
    selectedIndex = e.target.value;
    await loadViz(selectedIndex);
  }

  // ─── derived ────────────────────────────────────────────────────────────────

  let statusColor = $derived(
    report?.status === 'ok'         ? '#22c55e'
    : report?.status === 'degraded' ? '#f59e0b'
    : '#ef4444'
  );

  /** Group entities by kind for a compact summary bar. */
  let kindCounts = $derived.by(() => {
    const counts = {};
    for (const e of entities) {
      const k = e.kind ?? 'Unknown';
      counts[k] = (counts[k] ?? 0) + 1;
    }
    return Object.entries(counts).sort((a, b) => b[1] - a[1]);
  });

  /** SVG node-link layout: simple layered vertical layout. */
  const SVG_W = 700;
  const SVG_H = 420;

  let vizNodes = $derived.by(() => {
    // clusters is ClusterResponse: { k, clusters: [{id, label, members, size, cohesion}] }
    const clusterItems = clusters?.clusters ?? [];
    if (!clusterItems.length && !entities.length) return { nodes: [], edges: [] };

    // Use cluster labels if available, otherwise top-50 entities.
    const raw = clusterItems.length
      ? clusterItems.slice(0, 40).map(c => ({ id: String(c.id), name: c.label, kind: 'Module' }))
      : entities.slice(0, 40);

    const count = raw.length;
    if (count === 0) return { nodes: [], edges: [] };

    const cols = Math.ceil(Math.sqrt(count));
    const rows = Math.ceil(count / cols);
    const padX = 70;
    const padY = 50;
    const cellW = (SVG_W - padX * 2) / Math.max(cols - 1, 1);
    const cellH = (SVG_H - padY * 2) / Math.max(rows - 1, 1);

    const nodes = raw.map((item, i) => {
      const col = i % cols;
      const row = Math.floor(i / cols);
      return {
        id: item.id ?? item.name ?? String(i),
        label: truncate(item.name ?? item.qualified_name ?? String(i), 20),
        kind: item.kind,
        x: padX + col * cellW,
        y: padY + row * cellH,
        isCluster: !!(clusters?.clusters?.length),
      };
    });

    return { nodes, edges: [] };
  });

  function truncate(str, n) {
    if (!str) return '';
    return str.length <= n ? str : str.slice(0, n - 1) + '…';
  }

  /** Colour by entity kind. */
  function kindColor(kind) {
    const map = {
      Function: '#7c3aed',
      Method: '#8b5cf6',
      Class: '#2563eb',
      Interface: '#0ea5e9',
      Module: '#10b981',
      File: '#64748b',
      Field: '#f59e0b',
      TestCase: '#ef4444',
      Import: '#6b7280',
      Export: '#6b7280',
    };
    return map[kind] ?? '#475569';
  }
</script>

<div class="tab-content">
  <h2 class="section-title">Trusty Analyze</h2>

  <!-- ── Health panel ──────────────────────────────────────────────────────── -->
  {#if metricsLoading}
    <div class="placeholder">Loading analyze metrics…</div>
  {:else if metricsError}
    <div class="not-available">{metricsError}</div>
  {:else if report}
    <div class="meta-row">
      <span class="badge" style="background: {statusColor}22; color: {statusColor}; border-color: {statusColor}44;">
        <span class="dot" style="background: {statusColor};"></span>
        {report.status}
      </span>
      <span class="version">v{report.version}</span>
    </div>

    <div class="stat-grid">
      <div class="stat-card">
        <span class="stat-value" style="color: {report.metrics?.search_reachable ? '#22c55e' : '#ef4444'};">
          {report.metrics?.search_reachable ? 'Yes' : 'No'}
        </span>
        <span class="stat-label">Search Reachable</span>
      </div>
      <div class="stat-card">
        <span class="stat-value">{indexes.length}</span>
        <span class="stat-label">Indexed Projects</span>
      </div>
    </div>
  {/if}

  <!-- ── Index selector ────────────────────────────────────────────────────── -->
  {#if indexesError}
    <div class="not-available">{indexesError}</div>
  {:else if indexesLoading}
    <div class="placeholder">Loading indexes…</div>
  {:else if indexes.length > 0}
    <div class="selector-row">
      <label class="selector-label" for="index-select">Project index:</label>
      <select id="index-select" class="index-select" onchange={onIndexChange} value={selectedIndex}>
        {#each indexes as idx (idx.id)}
          <option value={idx.id}>{idx.id}</option>
        {/each}
      </select>
    </div>
  {/if}

  <!-- ── Visualization ─────────────────────────────────────────────────────── -->
  {#if selectedIndex}
    {#if vizLoading}
      <div class="placeholder">Loading visualization…</div>
    {:else if vizError}
      <div class="not-available">{vizError}</div>
    {:else}
      <!-- Kind summary bar -->
      {#if kindCounts.length > 0}
        <div class="kind-bar">
          {#each kindCounts.slice(0, 8) as [kind, count] (kind)}
            <span class="kind-badge" style="background: {kindColor(kind)}22; color: {kindColor(kind)}; border-color: {kindColor(kind)}44;">
              {kind}: {count}
            </span>
          {/each}
        </div>
      {/if}

      <!-- SVG node map -->
      {#if vizNodes.nodes.length > 0}
        <h3 class="sub-title">
          {clusters?.clusters?.length ? 'Cluster Labels' : 'Top Entities'} — {vizNodes.nodes.length} nodes
        </h3>
        <div class="svg-wrap">
          <svg width={SVG_W} height={SVG_H} viewBox="0 0 {SVG_W} {SVG_H}" role="img" aria-label="Entity node map">
            {#each vizNodes.nodes as node (node.id)}
              <g class="viz-node" transform="translate({node.x},{node.y})">
                <circle r="10" fill={kindColor(node.kind)} fill-opacity="0.85" stroke="#2d3348" stroke-width="1.5"/>
                <text
                  x="0" y="22"
                  text-anchor="middle"
                  font-size="9"
                  fill="#94a3b8"
                  font-family="'JetBrains Mono', monospace"
                >
                  {node.label}
                </text>
              </g>
            {/each}
          </svg>
        </div>
      {/if}

      <!-- Entities table (top 50) -->
      {#if entities.length > 0}
        <h3 class="sub-title">Entities (top {Math.min(entities.length, 50)})</h3>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Kind</th>
                <th>Name</th>
                <th>File</th>
                <th>Language</th>
              </tr>
            </thead>
            <tbody>
              {#each entities.slice(0, 50) as ent (ent.id)}
                <tr>
                  <td><span class="kind-pill" style="color: {kindColor(ent.kind)};">{ent.kind}</span></td>
                  <td><code>{ent.name}</code></td>
                  <td class="path">{ent.file ?? '—'}</td>
                  <td class="lang">{ent.language ?? '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else if !vizLoading}
        <p class="empty-hint">No entities found for this index. The index may not have been analyzed yet.</p>
      {/if}

      <!-- Clusters summary (ClusterResponse: {k, method, dim, iterations, chunk_count, clusters}) -->
      {#if clusters?.clusters?.length > 0}
        <h3 class="sub-title">Clusters (k={clusters.k}, method={clusters.method})</h3>
        <div class="stat-grid clusters-grid">
          <div class="stat-card">
            <span class="stat-value">{clusters.k}</span>
            <span class="stat-label">Clusters (k)</span>
          </div>
          <div class="stat-card">
            <span class="stat-value">{clusters.chunk_count ?? '—'}</span>
            <span class="stat-label">Chunks Clustered</span>
          </div>
          <div class="stat-card">
            <span class="stat-value">{clusters.dim ?? '—'}</span>
            <span class="stat-label">Embedding Dim</span>
          </div>
          <div class="stat-card">
            <span class="stat-value">{clusters.iterations ?? '—'}</span>
            <span class="stat-label">k-Means Iterations</span>
          </div>
        </div>
        <!-- Cluster label list -->
        <div class="table-wrap">
          <table>
            <thead>
              <tr><th>#</th><th>Label</th><th>Size</th><th>Cohesion</th></tr>
            </thead>
            <tbody>
              {#each clusters.clusters as c (c.id)}
                <tr>
                  <td class="num">{c.id}</td>
                  <td>{c.label}</td>
                  <td class="num">{c.size}</td>
                  <td class="num">{(c.cohesion ?? 0).toFixed(3)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    {/if}
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

  .stat-grid {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; align-items: center; gap: 0.25rem;
  }
  .stat-value { font-size: 1.6rem; font-weight: 700; color: #e2e8f0; }
  .stat-label { font-size: 0.75rem; color: #94a3b8; text-transform: uppercase; letter-spacing: 0.05em; }

  .selector-row {
    display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.25rem;
  }
  .selector-label { color: #94a3b8; font-size: 0.85rem; }
  .index-select {
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.375rem;
    color: #e2e8f0; padding: 0.35rem 0.6rem; font-size: 0.85rem; cursor: pointer;
    max-width: 500px; min-width: 200px;
  }
  .index-select:focus { outline: none; border-color: #7c3aed; }

  .kind-bar {
    display: flex; flex-wrap: wrap; gap: 0.4rem; margin-bottom: 1rem;
  }
  .kind-badge {
    display: inline-block; font-size: 0.72rem; font-weight: 600;
    padding: 0.15rem 0.5rem; border-radius: 9999px; border: 1px solid;
  }

  .svg-wrap {
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.5rem;
    overflow: auto; margin-bottom: 1.25rem; padding: 0.5rem;
  }

  .viz-node { cursor: default; }
  .viz-node circle { transition: r 0.15s; }
  .viz-node:hover circle { r: 14; }

  .sub-title { font-size: 1rem; font-weight: 600; color: #94a3b8; margin: 0 0 0.75rem; }
  .table-wrap { overflow-x: auto; margin-bottom: 1.5rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  th {
    text-align: left; padding: 0.5rem 0.75rem;
    background: #1e2130; color: #94a3b8; font-weight: 600;
    border-bottom: 1px solid #2d3348;
  }
  td { padding: 0.5rem 0.75rem; border-bottom: 1px solid #1e2130; color: #e2e8f0; }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: #1e2130; }
  td.path { font-size: 0.78rem; color: #94a3b8; max-width: 300px; overflow: hidden; text-overflow: ellipsis; }
  td.lang { font-size: 0.78rem; color: #64748b; }
  .kind-pill { font-size: 0.75rem; font-weight: 600; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    background: #0f1117; padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }
  .empty-hint { color: #94a3b8; font-size: 0.85rem; }
  .clusters-grid { margin-top: 1rem; }
  td.num { text-align: right; font-variant-numeric: tabular-nums; }
</style>
