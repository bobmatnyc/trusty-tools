<script>
  import { onMount } from 'svelte';

  let report = $state(null);
  let loading = $state(true);
  let error = $state(null);

  onMount(async () => {
    try {
      const resp = await fetch('/api/console/metrics/memory');
      if (resp.status === 503) {
        error = 'trusty-memory metrics not yet available (daemon absent or first boot).';
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

  let statusColor = $derived(
    report?.status === 'ok'       ? '#22c55e'
    : report?.status === 'degraded' ? '#f59e0b'
    : '#ef4444'
  );
</script>

<div class="tab-content">
  <h2 class="section-title">Trusty Memory</h2>

  {#if loading}
    <div class="placeholder">Loading memory metrics…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else if report}
    <!-- Status badge + version -->
    <div class="meta-row">
      <span class="badge" style="background: {statusColor}22; color: {statusColor}; border-color: {statusColor}44;">
        <span class="dot" style="background: {statusColor};"></span>
        {report.status}
      </span>
      <span class="version">v{report.version}</span>
    </div>

    <!-- Aggregate stats -->
    <div class="stat-grid">
      <div class="stat-card">
        <span class="stat-value">{report.metrics?.palace_count ?? 0}</span>
        <span class="stat-label">Palaces</span>
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
              <th>KG Triples</th>
            </tr>
          </thead>
          <tbody>
            {#each report.metrics.palaces as p (p.id)}
              <tr>
                <td><code>{p.id}</code></td>
                <td>{p.name}</td>
                <td class="num">{p.drawer_count}</td>
                <td class="num">{p.vector_count}</td>
                <td class="num">{p.kg_triple_count}</td>
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
    display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 0.75rem; margin-bottom: 1.5rem;
  }
  .stat-card {
    background: #1e2130; border: 1px solid #2d3348; border-radius: 0.5rem;
    padding: 1rem; display: flex; flex-direction: column; align-items: center; gap: 0.25rem;
  }
  .stat-value { font-size: 1.6rem; font-weight: 700; color: #e2e8f0; }
  .stat-label { font-size: 0.75rem; color: #94a3b8; text-transform: uppercase; letter-spacing: 0.05em; }

  .sub-title { font-size: 1rem; font-weight: 600; color: #94a3b8; margin: 0 0 0.75rem; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  th {
    text-align: left; padding: 0.5rem 0.75rem;
    background: #1e2130; color: #94a3b8; font-weight: 600;
    border-bottom: 1px solid #2d3348;
  }
  td { padding: 0.5rem 0.75rem; border-bottom: 1px solid #1e2130; color: #e2e8f0; }
  tr:last-child td { border-bottom: none; }
  tr:hover td { background: #1e2130; }
  td.num { text-align: right; font-variant-numeric: tabular-nums; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    background: #0f1117; padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }
  .empty-hint { color: #94a3b8; font-size: 0.85rem; }
</style>
