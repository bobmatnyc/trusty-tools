<script>
  /*
   * Why: Operators need to see which smell categories dominate a corpus and
   * drill into specific offenders. A bar chart on category counts surfaces
   * the prevailing problem class at a glance.
   * What: D3 bar chart of smell-category counts + a filterable detail list.
   * Test: Select an index, expect bars for each category; click a category
   * and the list filters to that smell name.
   */
  import * as d3 from 'd3';
  import { onMount, onDestroy, tick } from 'svelte';
  import {
    getSelectedIndex,
    getSmells,
    refreshSmells,
    getTheme
  } from '../state.svelte.js';

  let selected = $derived(getSelectedIndex());
  let smells = $derived(getSmells());
  let category = $state('');
  let chartEl = $state(null);

  $effect(() => {
    if (!selected) return;
    refreshSmells(selected, category || undefined).catch(() => {});
  });

  /*
   * Why: API may return one row per chunk with a `smells: [...]` array, or
   * pre-flattened rows; we normalize to a flat list of {category, chunk}.
   * What: Returns Array<{ category, chunk }>.
   * Test: Pass [{ smells: [{ category: 'long_function' }], file: 'a.rs' }]
   * and expect [{ category: 'long_function', chunk: {...} }].
   */
  function flatten(rows) {
    const out = [];
    for (const r of rows || []) {
      if (Array.isArray(r.smells) && r.smells.length) {
        for (const s of r.smells) {
          out.push({ category: s.category || s.name || 'unknown', chunk: r, smell: s });
        }
      } else if (r.category || r.name) {
        out.push({ category: r.category || r.name, chunk: r, smell: r });
      }
    }
    return out;
  }

  let flat = $derived(flatten(smells));
  let counts = $derived.by(() => {
    const m = new Map();
    for (const f of flat) m.set(f.category, (m.get(f.category) || 0) + 1);
    return [...m.entries()].map(([category, count]) => ({ category, count }))
      .sort((a, b) => b.count - a.count);
  });
  let filtered = $derived(category ? flat.filter((f) => f.category === category) : flat);

  function renderBars() {
    if (!chartEl) return;
    d3.select(chartEl).selectAll('*').remove();
    const data = counts;
    if (!data.length) return;
    const cs = getComputedStyle(document.documentElement);
    const color = {
      subtext: cs.getPropertyValue('--subtext').trim(),
      border: cs.getPropertyValue('--border').trim(),
      text: cs.getPropertyValue('--text').trim(),
      mauve: cs.getPropertyValue('--mauve').trim()
    };

    const width = chartEl.clientWidth || 800;
    const height = 280;
    const margin = { top: 16, right: 16, bottom: 60, left: 48 };
    const innerW = width - margin.left - margin.right;
    const innerH = height - margin.top - margin.bottom;

    const x = d3.scaleBand().domain(data.map((d) => d.category)).range([0, innerW]).padding(0.2);
    const y = d3.scaleLinear().domain([0, d3.max(data, (d) => d.count) || 1]).nice().range([innerH, 0]);

    const svg = d3
      .select(chartEl)
      .append('svg')
      .attr('width', width)
      .attr('height', height)
      .attr('viewBox', `0 0 ${width} ${height}`);

    const g = svg.append('g').attr('transform', `translate(${margin.left},${margin.top})`);

    g.append('g')
      .attr('transform', `translate(0,${innerH})`)
      .call(d3.axisBottom(x))
      .selectAll('text')
      .style('fill', color.subtext)
      .style('font-size', '11px')
      .attr('transform', 'rotate(-25)')
      .attr('text-anchor', 'end')
      .attr('dx', '-0.4em')
      .attr('dy', '0.6em');

    g.append('g')
      .call(d3.axisLeft(y).ticks(5))
      .selectAll('text')
      .style('fill', color.subtext)
      .style('font-size', '11px');

    g.selectAll('.domain, .tick line').style('stroke', color.border);

    g.selectAll('rect.bar')
      .data(data)
      .join('rect')
      .attr('class', 'bar')
      .attr('x', (d) => x(d.category))
      .attr('y', (d) => y(d.count))
      .attr('width', x.bandwidth())
      .attr('height', (d) => innerH - y(d.count))
      .attr('fill', color.mauve)
      .attr('fill-opacity', 0.85)
      .style('cursor', 'pointer')
      .on('click', (_e, d) => {
        category = category === d.category ? '' : d.category;
      });

    g.selectAll('text.value')
      .data(data)
      .join('text')
      .attr('class', 'value')
      .attr('x', (d) => x(d.category) + x.bandwidth() / 2)
      .attr('y', (d) => y(d.count) - 4)
      .attr('text-anchor', 'middle')
      .style('fill', color.text)
      .style('font-size', '11px')
      .style('font-weight', '600')
      .text((d) => d.count);
  }

  let ro;
  onMount(async () => {
    await tick();
    renderBars();
    ro = new ResizeObserver(() => renderBars());
    if (chartEl) ro.observe(chartEl);
  });
  onDestroy(() => ro && ro.disconnect());

  $effect(() => {
    counts;
    getTheme();
    if (chartEl) renderBars();
  });
</script>

<h1 class="page-title">Smells</h1>

{#if !selected}
  <div class="card"><div class="empty">Select an index in the top bar.</div></div>
{:else}
  <div class="card mb-4">
    <div class="card-header flex-between">
      <span>By Category</span>
      {#if category}
        <button class="btn btn-sm" onclick={() => (category = '')}>clear filter: {category}</button>
      {/if}
    </div>
    <div class="card-body">
      <div bind:this={chartEl} class="bar-chart"></div>
      {#if counts.length === 0}
        <div class="empty">No smells detected for this index.</div>
      {/if}
    </div>
  </div>

  <div class="card">
    <div class="card-header">
      Detail {filtered.length ? `(${filtered.length})` : ''}
    </div>
    <div class="card-body" style="padding: 0">
      {#if filtered.length === 0}
        <div class="empty">No matching smells.</div>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th>Category</th>
              <th>Function</th>
              <th>File</th>
              <th>Lines</th>
              <th>Severity</th>
            </tr>
          </thead>
          <tbody>
            {#each filtered.slice(0, 200) as f}
              {@const c = f.chunk || {}}
              {@const sev = (f.smell?.severity || '').toString()}
              <tr>
                <td><span class="badge">{f.category}</span></td>
                <td class="text-mono text-xs">{c.function_name || '—'}</td>
                <td class="text-muted text-xs truncate" style="max-width: 320px">{c.file || '—'}</td>
                <td class="text-xs text-muted">{c.start_line ?? '?'}–{c.end_line ?? '?'}</td>
                <td>
                  {#if sev}
                    <span class="badge sev-{sev.toLowerCase()}">{sev}</span>
                  {:else}
                    <span class="text-muted text-xs">—</span>
                  {/if}
                </td>
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
  .bar-chart {
    width: 100%;
    min-height: 280px;
  }
</style>
