<script>
  import AppShell from '../../lib/AppShell.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';
  import { searchNav } from './data.js';

  const engines = [
    { label: 'BM25 ENGINE', value: '2.1ms', meta: 'p50 query latency · 24h', tone: 'success', status: 'OK', bars: [35, 50, 40, 65, 45, 55, 38, 48] },
    { label: 'VECTOR INDEX', value: '6.8ms', meta: 'p50 ANN lookup · 24h', tone: 'success', status: 'OK', bars: [55, 60, 48, 70, 62, 52, 58, 60] },
    { label: 'KNOWLEDGE GRAPH', value: '41ms', meta: 'expansion, cache cold', tone: 'warning', status: 'DEGRADED', warn: true, bars: [30, 35, 32, 45, 68, 85, 92, 100] },
    { label: 'EMBEDDER', value: '312/s', meta: 'chunks embedded', tone: 'success', status: 'OK', bars: [60, 72, 66, 80, 74, 70, 76, 72] }
  ];
  const sidecars = [
    { name: 'trusty-analyze', port: ':7879', tone: 'success', status: 'CONNECTED' },
    { name: 'trusty-embedderd', port: ':7881', tone: 'success', status: 'CONNECTED' },
    { name: 'trusty-bm25-daemon', port: ':7880', tone: 'danger', status: 'UNREACHABLE' },
    { name: 'trusty-memory', port: ':7882', tone: 'muted', status: 'NOT INSTALLED' }
  ];
  const events = [
    { t: '09:14', dot: '#5c9a3d', text: 'Reindex complete — memory-palace' },
    { t: '09:02', dot: '#c2331f', text: 'bm25-daemon connection lost' },
    { t: '08:47', dot: '#b07d10', text: 'KG cache evicted (memory pressure)' },
    { t: '06:00', dot: '#5c9a3d', text: 'Daemon started · tier 3 auto-selected' }
  ];
  const qpm = [30, 42, 38, 55, 70, 100, 84, 62, 48, 52, 44, 36];
</script>

{#snippet actions()}
  <Button variant="danger" size="sm">STOP</Button>
  <Button size="sm">RESTART</Button>
{/snippet}

<AppShell sidebar={searchNav('Health')} crumb="HEALTH" topbarActions={actions}>
  <div class="head">
    <h1 class="page-title">HEALTH</h1>
    <span class="refresh">LAST CHECK 09:14:32 · AUTO-REFRESH 10S</span>
  </div>

  <div class="grid four">
    {#each engines as e}
      <div class="stat">
        <div class="row">
          <span class="stat-label label">{e.label}</span>
          <Badge tone={e.tone}>{e.status}</Badge>
        </div>
        <div class="val" class:warn={e.warn}>{e.value}</div>
        <div class="spark">
          {#each e.bars as h, i}
            <span style="height:{h}%; background:{i === e.bars.length - 1 ? (e.warn ? '#b07d10' : 'var(--trusty-accent)') : 'var(--trusty-border)'};"></span>
          {/each}
        </div>
        <div class="stat-meta">{e.meta}</div>
      </div>
    {/each}
  </div>

  <div class="grid two">
    <div class="card">
      <div class="card-header">MEMORY TIER</div>
      <div class="card-body">
        <div class="tier-row">
          <span class="tier">TIER 3 — 32 GB MACHINE</span>
          <span class="budget">11.2 GB / 14 GB BUDGET</span>
        </div>
        <div class="segbar">
          <span style="width:46%; background:#b7410e;"></span>
          <span style="width:21%; background:#d97742;"></span>
          <span style="width:13%; background:#e9b98a;"></span>
          <span class="rest"></span>
        </div>
        <div class="legend">
          <span><i style="background:#b7410e"></i>HNSW 6.4 GB</span>
          <span><i style="background:#d97742"></i>BM25 2.9 GB</span>
          <span><i style="background:#e9b98a"></i>CACHE 1.9 GB</span>
        </div>
        <div class="qpm">
          <div class="qpm-head"><span class="stat-label">QUERIES / MIN · LAST HOUR</span><span class="peak">PEAK 240</span></div>
          <div class="qpm-bars">
            {#each qpm as h}
              <span style="height:{h}%; background:{h === 100 ? '#b7410e' : h >= 55 ? '#d97742' : '#e9b98a'};"></span>
            {/each}
          </div>
        </div>
      </div>
    </div>
    <div class="card">
      <div class="card-header">SIDECARS</div>
      <table class="table">
        <tbody>
          {#each sidecars as s}
            <tr>
              <td class="name">{s.name}</td>
              <td class="port">{s.port}</td>
              <td class="right"><Badge tone={s.tone}>{s.status}</Badge></td>
            </tr>
          {/each}
        </tbody>
      </table>
      <div class="events">
        <div class="stat-label">RECENT EVENTS</div>
        {#each events as ev}
          <div class="ev">
            <span class="dot" style="background:{ev.dot}"></span>
            <span class="time">{ev.t}</span>
            <span class="text">{ev.text}</span>
          </div>
        {/each}
      </div>
    </div>
  </div>
</AppShell>

<style>
  .head { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 24px; }
  h1 { font-size: 28px; margin: 0; }
  .refresh { font: 500 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .grid { display: grid; gap: 16px; }
  .four { grid-template-columns: repeat(4, 1fr); margin-bottom: 20px; }
  .two { grid-template-columns: 1.2fr 1fr; }
  .row { display: flex; justify-content: space-between; align-items: center; margin-bottom: 12px; }
  .label { margin-bottom: 0; }
  .val { font: 700 24px var(--trusty-display); }
  .val.warn { color: var(--trusty-warning); }
  .spark { display: flex; align-items: flex-end; gap: 2px; height: 22px; margin-top: 10px; }
  .spark span { width: 6px; display: block; }
  .tier-row { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 10px; }
  .tier { font: 600 13px var(--trusty-mono); }
  .budget { font: 500 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .segbar { display: flex; height: 14px; border: 1px solid var(--trusty-border); border-radius: 2px; overflow: hidden; }
  .segbar span { display: block; }
  .segbar .rest { flex: 1; background: var(--trusty-surface-raised); }
  .legend { display: flex; gap: 20px; margin-top: 14px; }
  .legend span { display: flex; align-items: center; gap: 7px; font: 500 11px var(--trusty-mono); color: var(--trusty-text-secondary); }
  .legend i { width: 9px; height: 9px; display: block; }
  .qpm { margin-top: 18px; padding-top: 16px; border-top: 1px solid var(--trusty-surface-raised); }
  .qpm-head { display: flex; justify-content: space-between; margin-bottom: 8px; }
  .peak { font: 600 11px var(--trusty-mono); color: var(--trusty-accent); }
  .qpm-bars { display: flex; align-items: flex-end; gap: 3px; height: 48px; }
  .qpm-bars span { flex: 1; display: block; }
  .name { font-weight: 600; }
  .port { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .right { text-align: right; }
  .events { padding: 16px 20px; border-top: 1px solid var(--trusty-surface-raised); }
  .events .stat-label { margin-bottom: 12px; }
  .ev { display: flex; gap: 10px; align-items: center; margin-bottom: 10px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; flex: none; }
  .time { font: 500 11px var(--trusty-mono); color: var(--trusty-text-muted); flex: 0 0 52px; }
  .text { font-size: 12.5px; color: var(--trusty-text-secondary); }
</style>
