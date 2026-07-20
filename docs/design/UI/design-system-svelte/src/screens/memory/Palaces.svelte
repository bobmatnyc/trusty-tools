<script>
  import AppShell from '../../lib/AppShell.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';
  import { memoryNav } from '../search/data.js';

  let palaces = $state([
    {
      name: 'trusty-tools', drawers: 142, active: 'active 2m ago', open: true,
      items: [
        { kind: 'MESSAGE', tone: 'info', text: 'Reindex schedule agreed with trusty-search: nightly at 02:00, skip if…', when: 'unread' },
        { kind: 'DECISION', tone: 'muted', text: 'Chose usearch over hnswlib for the vector index — better mmap story…', when: 'Jul 12' },
        { kind: 'SNIPPET', tone: 'muted', text: 'RRF fusion constant k=60 tuned against the eval set; see evals/rrf_sweep…', when: 'Jul 09' }
      ]
    },
    { name: 'memory-palace', drawers: 96, active: 'active 1h ago', open: false, items: [] },
    { name: 'gitflow-rs', drawers: 61, active: 'active yesterday', open: false, items: [] },
    { name: 'docs-site', drawers: 38, active: 'active Jul 14', open: false, items: [] }
  ]);
</script>

<AppShell sidebar={memoryNav('Palaces')} crumb="PALACES" status="ONLINE v0.3.8">
  <div class="head">
    <h1 class="page-title">PALACES</h1>
    <span class="count">88 PALACES · 4,312 DRAWERS</span>
  </div>
  <div class="filters">
    <input class="input grow" placeholder="filter by name or project…">
    <select class="select auto"><option>SORT: ACTIVITY</option><option>NAME</option><option>DRAWERS</option><option>CREATED</option></select>
    <select class="select auto"><option>ALL COLLECTIONS</option><option>trusty-*</option></select>
    <Button size="sm">GROUP BY PROJECT</Button>
  </div>
  <div class="card tree">
    {#each palaces as p}
      <button class="node" class:open={p.open} onclick={() => (p.open = !p.open)}>
        <span class="caret">{p.open ? '▾' : '▸'}</span>
        <span class="pname">{p.name}</span>
        <Badge tone="muted">{p.drawers} DRAWERS</Badge>
        <span class="when">{p.active}</span>
        <a href="#graph" onclick={(e) => e.stopPropagation()}>GRAPH →</a>
      </button>
      {#if p.open && p.items.length}
        <div class="drawers">
          {#each p.items as d}
            <div class="drawer">
              <Badge tone={d.tone}>{d.kind}</Badge>
              <span class="text">{d.text}</span>
              <span class="dwhen">{d.when}</span>
            </div>
          {/each}
        </div>
      {/if}
    {/each}
  </div>
</AppShell>

<style>
  .head { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 20px; }
  h1 { font-size: 28px; margin: 0; }
  .count { font: 500 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .filters { display: flex; gap: 8px; margin-bottom: 16px; }
  .grow { flex: 1; width: auto; }
  .auto { width: auto; padding: 8px 12px; font-size: 12px; }
  .tree { overflow: hidden; }
  .node { display: flex; align-items: center; gap: 12px; width: 100%; text-align: left; padding: 13px 20px; border: none; border-bottom: 1px solid var(--trusty-surface-hover); background: none; font: inherit; color: inherit; cursor: pointer; }
  .node:hover { background: var(--trusty-surface-hover); }
  .node.open { background: var(--trusty-primary-soft); }
  .caret { font: 600 12px var(--trusty-mono); color: var(--trusty-text-muted); }
  .open .caret { color: var(--trusty-accent); }
  .pname { font-weight: 600; flex: 1; }
  .when { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .node a { font: 600 11px var(--trusty-mono); }
  .drawers { padding: 6px 20px 14px 46px; display: flex; flex-direction: column; gap: 8px; border-bottom: 1px solid var(--trusty-surface-hover); }
  .drawer { display: flex; gap: 10px; align-items: center; }
  .text { font-size: 12.5px; color: var(--trusty-text-secondary); flex: 1; }
  .dwhen { font: 400 10px var(--trusty-mono); color: var(--trusty-text-muted); }
</style>
