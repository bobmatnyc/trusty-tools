<script>
  /*
   * Why: A treemap is the densest visual for "which files are the worst?" —
   * size encodes total cyclomatic complexity (riskiness) and color encodes
   * grade, so an analyst sees both volume and severity in one glance.
   * What: Aggregates hotspot chunks by file → renders a D3 treemap. Clicking
   * a tile opens a detail panel with file path, line range, function name,
   * and metrics. A top-20 table sits below for sortable raw access.
   * Test: Select an index with known hotspots; expect red tiles for grade F
   * files and green for grade A. Click a tile — detail panel populates.
   */
  import * as d3 from 'd3';
  import { onMount, onDestroy, tick } from 'svelte';
  import {
    getSelectedIndex,
    getHotspots,
    refreshHotspots,
    getTheme
  } from '../state.svelte.js';

  let selected = $derived(getSelectedIndex());
  let hotspots = $derived(getHotspots());

  let containerEl = $state(null);
  let detail = $state(null);
  let topK = $state(50);

  // Re-fetch when the selected index changes or topK changes.
  $effect(() => {
    if (!selected) return;
    refreshHotspots(selected, topK).catch(() => {});
  });

  /*
   * Why: D3 treemap needs a hierarchy. We group chunk-level hotspots by their
   * `file` and sum cyclomatic complexity so the rectangle area reflects the
   * file's overall complexity weight. The grade with the highest count per
   * file drives the color.
   * What: Builds { name, children: [{ name: file, value, grade, chunks }] }.
   * Test: Pass two chunks for "a.rs" (cyclo=5 and cyclo=3), expect a single
   * "a.rs" leaf with value=8.
   */
  function buildHierarchy(rows) {
    const byFile = new Map();
    for (const h of rows) {
      const file = h.file || 'unknown';
      const m = h.metrics || {};
      const cyclo = Number(m.cyclomatic ?? h.cyclomatic ?? 1);
      const grade = (h.grade || m.grade || 'C').toString().toUpperCase();
      let entry = byFile.get(file);
      if (!entry) {
        entry = { name: file, value: 0, grades: {}, chunks: [] };
        byFile.set(file, entry);
      }
      entry.value += Math.max(1, cyclo);
      entry.grades[grade] = (entry.grades[grade] || 0) + 1;
      entry.chunks.push(h);
    }
    const leaves = [...byFile.values()].map((e) => {
      // Worst grade among chunks dominates display color.
      const order = ['F', 'D', 'C', 'B', 'A'];
      let worst = 'A';
      for (const g of order) {
        if (e.grades[g]) {
          worst = g;
          break;
        }
      }
      return { name: e.name, value: e.value, grade: worst, chunks: e.chunks };
    });
    return { name: 'root', children: leaves };
  }

  /*
   * Why: SVG fill/stroke attributes can't reference CSS variables directly in
   * all browsers when used inside d3-generated nodes, and we want re-render on
   * theme change. Resolve var(--grade-*) to literal hex via getComputedStyle.
   * What: Returns the current theme's hex for each grade letter.
   * Test: setTheme('light'), call gradeColors().A — expect Foundry v2 success (green) hex.
   */
  function gradeColors() {
    const cs = getComputedStyle(document.documentElement);
    const v = (name) => cs.getPropertyValue(name).trim();
    return {
      A: v('--grade-a'),
      B: v('--grade-b'),
      C: v('--grade-c'),
      D: v('--grade-d'),
      F: v('--grade-f'),
      bg: v('--bg'),
      inverse: v('--text-inverse')
    };
  }

  function render() {
    if (!containerEl) return;
    d3.select(containerEl).selectAll('*').remove();
    const data = buildHierarchy(hotspots);
    if (!data.children.length) return;
    const gradeColor = gradeColors();

    const width = containerEl.clientWidth || 800;
    const height = 480;

    const root = d3
      .hierarchy(data)
      .sum((d) => d.value || 0)
      .sort((a, b) => (b.value || 0) - (a.value || 0));

    d3.treemap().size([width, height]).padding(2).round(true)(root);

    const svg = d3
      .select(containerEl)
      .append('svg')
      .attr('width', width)
      .attr('height', height)
      .attr('viewBox', `0 0 ${width} ${height}`)
      .style('font-family', 'var(--trusty-font)');

    const nodes = svg
      .selectAll('g')
      .data(root.leaves())
      .join('g')
      .attr('transform', (d) => `translate(${d.x0},${d.y0})`)
      .style('cursor', 'pointer')
      .on('click', (_e, d) => {
        detail = d.data;
      });

    nodes
      .append('rect')
      .attr('width', (d) => Math.max(0, d.x1 - d.x0))
      .attr('height', (d) => Math.max(0, d.y1 - d.y0))
      .attr('fill', (d) => gradeColor[d.data.grade] || gradeColor.C)
      .attr('fill-opacity', 0.7)
      .attr('stroke', gradeColor.bg)
      .attr('stroke-width', 1);

    nodes
      .append('text')
      .attr('x', 6)
      .attr('y', 16)
      .attr('fill', gradeColor.inverse)
      .style('font-size', '11px')
      .style('font-weight', '600')
      .style('pointer-events', 'none')
      .each(function (d) {
        const w = d.x1 - d.x0;
        const h = d.y1 - d.y0;
        if (w < 50 || h < 24) return;
        const short = (d.data.name || '').split('/').pop();
        const txt = d3.select(this);
        txt.append('tspan').text(short);
        if (h > 40) {
          txt
            .append('tspan')
            .attr('x', 6)
            .attr('y', 32)
            .style('font-weight', '400')
            .style('font-size', '10px')
            .text(`cyclo ${d.data.value} • ${d.data.grade}`);
        }
      });
  }

  let resizeObserver;
  onMount(async () => {
    await tick();
    render();
    resizeObserver = new ResizeObserver(() => render());
    if (containerEl) resizeObserver.observe(containerEl);
  });

  onDestroy(() => {
    if (resizeObserver) resizeObserver.disconnect();
  });

  $effect(() => {
    // Re-render whenever the hotspots array or theme changes.
    hotspots;
    getTheme();
    if (containerEl) render();
  });

  let table20 = $derived(hotspots.slice(0, 20));
</script>

<h1 class="page-title">Complexity</h1>

{#if !selected}
  <div class="card"><div class="empty">Select an index in the top bar.</div></div>
{:else}
  <div class="card mb-4">
    <div class="card-header flex-between">
      <span>File Treemap</span>
      <label class="text-xs text-muted" style="display: flex; align-items: center; gap: 8px">
        Top K
        <input
          class="input"
          type="number"
          min="10"
          max="500"
          style="width: 90px"
          bind:value={topK}
        />
      </label>
    </div>
    <div class="card-body">
      <div class="legend">
        <span><span class="swatch" style="background: var(--grade-a)"></span> A</span>
        <span><span class="swatch" style="background: var(--grade-b)"></span> B</span>
        <span><span class="swatch" style="background: var(--grade-c)"></span> C</span>
        <span><span class="swatch" style="background: var(--grade-d)"></span> D</span>
        <span><span class="swatch" style="background: var(--grade-f)"></span> F</span>
      </div>
      <div bind:this={containerEl} class="treemap-container"></div>
      {#if detail}
        <div class="detail-panel">
          <div class="flex-between">
            <strong class="text-mono">{detail.name}</strong>
            <button class="btn btn-sm" onclick={() => (detail = null)}>close</button>
          </div>
          <div class="text-xs text-muted mt-3">
            Total cyclomatic: <strong>{detail.value}</strong> • Grade <strong>{detail.grade}</strong> •
            {detail.chunks.length} chunks
          </div>
          <table class="table mt-3">
            <thead>
              <tr><th>Function</th><th>Lines</th><th>Cyclo</th><th>Cognitive</th><th>Grade</th></tr>
            </thead>
            <tbody>
              {#each detail.chunks as c}
                {@const m = c.metrics || {}}
                <tr>
                  <td class="text-mono text-xs">{c.function_name || '—'}</td>
                  <td class="text-xs text-muted">{c.start_line ?? c.line_start ?? '?'}–{c.end_line ?? c.line_end ?? '?'}</td>
                  <td>{m.cyclomatic ?? '—'}</td>
                  <td>{m.cognitive ?? '—'}</td>
                  <td><span class="badge grade-{(c.grade || m.grade || 'C').toString().toLowerCase()}">{c.grade || m.grade || 'C'}</span></td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </div>
  </div>

  <div class="card">
    <div class="card-header">Top 20 Hotspots</div>
    <div class="card-body" style="padding: 0">
      {#if table20.length === 0}
        <div class="empty">No hotspots.</div>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th>Function</th>
              <th>File</th>
              <th>Lines</th>
              <th>Cyclo</th>
              <th>Cognitive</th>
              <th>Grade</th>
            </tr>
          </thead>
          <tbody>
            {#each table20 as h}
              {@const m = h.metrics || {}}
              {@const g = (h.grade || m.grade || '?').toString()}
              <tr>
                <td class="text-mono text-xs">{h.function_name || '—'}</td>
                <td class="text-muted text-xs truncate" style="max-width: 320px">{h.file || '—'}</td>
                <td class="text-xs text-muted">{h.start_line ?? '?'}–{h.end_line ?? '?'}</td>
                <td><strong>{m.cyclomatic ?? '—'}</strong></td>
                <td>{m.cognitive ?? '—'}</td>
                <td><span class="badge grade-{g.toLowerCase()}">{g}</span></td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  </div>
{/if}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .treemap-container {
    width: 100%;
    min-height: 480px;
  }
  .legend {
    display: flex;
    gap: 16px;
    margin-bottom: var(--trusty-space-3);
    color: var(--trusty-text-muted);
    font-size: var(--trusty-fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .swatch {
    display: inline-block;
    width: 12px;
    height: 12px;
    border-radius: 3px;
    margin-right: 6px;
    vertical-align: -2px;
  }
  .detail-panel {
    margin-top: var(--trusty-space-4);
    padding: var(--trusty-space-4);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--trusty-radius);
  }
</style>
