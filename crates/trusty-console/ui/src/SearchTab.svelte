<script>
  import { onMount } from 'svelte';

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

  onMount(async () => {
    try {
      const resp = await fetch('/api/console/metrics/search');
      if (resp.status === 503) {
        error = 'trusty-search metrics not yet available (daemon absent or first boot).';
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
    report?.status === 'ok'         ? '#22c55e'
    : report?.status === 'degraded' ? '#f59e0b'
    : '#ef4444'
  );

  let degradedColor = $derived(
    report?.metrics?.warm_boot_degraded ? '#f59e0b' : '#22c55e'
  );
</script>

<div class="tab-content">
  <h2 class="section-title">Trusty Search</h2>

  {#if loading}
    <div class="placeholder">Loading search metrics…</div>
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
        <span class="stat-value">{report.metrics?.index_count ?? 0}</span>
        <span class="stat-label">Indexes</span>
      </div>
      <div class="stat-card" style="border-color: {degradedColor}44;">
        <span class="stat-value" style="color: {degradedColor};">
          {report.metrics?.warm_boot_degraded ? 'Yes' : 'No'}
        </span>
        <span class="stat-label">Warm Boot Degraded</span>
      </div>
    </div>

    <!-- Per-index table -->
    {#if report.metrics?.indexes?.length > 0}
      <h3 class="sub-title">Indexes</h3>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Root Path</th>
              <th>Size</th>
            </tr>
          </thead>
          <tbody>
            {#each report.metrics.indexes as idx (idx.id)}
              <tr>
                <td><code>{idx.id ?? '—'}</code></td>
                <td class="path">{idx.root_path ?? '—'}</td>
                <td class="num">
                  {idx.size_bytes != null ? formatBytes(idx.size_bytes) : '—'}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="empty-hint">No indexes registered.</p>
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
  td.path { font-size: 0.8rem; color: #94a3b8; max-width: 400px; overflow: hidden; text-overflow: ellipsis; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.8rem;
    background: #0f1117; padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }
  .empty-hint { color: #94a3b8; font-size: 0.85rem; }
</style>
