<!--
  The Disk view — projects and worktrees, coloured by staleness (#6929).

  Why: DOC-73 §16 puts a DaisyDisk-shaped view of the workspace in the console,
  so an operator can see which worktrees are holding disk and which are safe to
  reclaim, before deciding anything. §13's ruling makes it DISPLAY ONLY: there
  is no clear button anywhere in this file, and none of the routes it calls can
  remove anything. #6930 owns the clear action.
  What: one `GET /api/console/disk/tree` per load, rendered as a sunburst above
  600px and as the same rows in a sorted list below it. The detail panel reads
  the row already in hand — no second request — so the chart and the panel
  cannot disagree. The keep-list is surfaced, not swallowed: an unreadable
  config is a banner, and a pattern that would not parse is a warning row.
  Test: `diskSunburst.test.js` covers the tier mapping, the collapse threshold
  and the fallback ordering; `tests/disk_mcp_bridge.rs` covers the route.
-->
<script>
  import { onMount } from 'svelte';
  import Badge from './Badge.svelte';
  import RefreshHeader from './RefreshHeader.svelte';
  import DiskSunburst from './DiskSunburst.svelte';
  import DiskDetail from './DiskDetail.svelte';
  import {
    TIERS,
    flatRows,
    formatBytes,
    isNarrow,
    sunburstRings,
    tierColor,
    tierLabel,
    tierTone,
  } from './diskSunburst.js';

  let survey = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);
  /** The container's width, so the fallback is chosen from the real layout. */
  let width = $state(0);
  let selected = $state(null);
  let sortKey = $state('bytes');
  let sortDir = $state('desc');

  /**
   * What went wrong, in the operator's terms rather than the number's.
   *
   * Why (#6929): this used to print `HTTP ${resp.status}` for everything except
   * a 503, so the live failure read `HTTP 502` — a number that names no cause
   * and suggests no action, for a response that also carried no body. The route
   * now sends `{status, hint}` on both failure arms; the hint is preferred and
   * the status only names the family when one is missing.
   */
  async function failureMessage(resp) {
    let hint = null;
    try {
      hint = (await resp.json())?.hint ?? null;
    } catch {
      hint = null;
    }
    if (hint) return `${hint}.`;
    if (resp.status === 503)
      return 'trusty-mpm is not reachable — the disk survey needs its MCP bridge.';
    if (resp.status === 502)
      return 'trusty-mpm did not answer the disk survey — check the daemon’s log.';
    return `The disk survey failed (HTTP ${resp.status}).`;
  }

  /**
   * Read the survey.
   *
   * The route always sends a classification budget under the console's 30 s MCP
   * call timeout, so a large fleet answers with some rows marked `review`
   * rather than timing out with none — see `routes::disk`.
   */
  async function load(isRefresh = false) {
    if (refreshing || (isRefresh && loading)) return;
    if (isRefresh) refreshing = true;
    else loading = true;
    try {
      const resp = await fetch('/api/console/disk/tree');
      if (!resp.ok) {
        error = await failureMessage(resp);
        return;
      }
      survey = await resp.json();
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  onMount(load);

  let narrow = $derived(isNarrow(width));
  let rings = $derived(survey ? sunburstRings(survey) : null);
  let rows = $derived(survey ? flatRows(survey, { key: sortKey, direction: sortDir }) : []);
  let counts = $derived(survey?.root?.counts ?? null);
  let keepList = $derived(survey?.keep_list ?? null);

  /** Cycle a column header between descending and ascending. */
  function sortBy(key) {
    if (sortKey === key) sortDir = sortDir === 'desc' ? 'asc' : 'desc';
    else {
      sortKey = key;
      sortDir = 'desc';
    }
  }

  /** Select a list row, using the same segment shape the sunburst emits. */
  function selectRow(row) {
    selected = {
      kind: row.kind,
      id: row.id,
      name: row.name,
      tier: row.tier,
      bytes: row.bytes,
      node: row.node,
    };
  }
</script>

<!-- `.foundry` is what scopes the ported Badge tone classes in foundry.css;
     without it every tier badge renders as unstyled text. -->
<div class="foundry tab-content" bind:clientWidth={width}>
  <RefreshHeader title="Disk" onRefresh={() => load(true)} {refreshing} />

  {#if loading}
    <div class="placeholder">Surveying projects and worktrees…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else if survey}
    <!-- The keep-list state, stated rather than swallowed. An unreadable config
         keeps EVERYTHING, so an operator who cannot see the failure would read
         a fleet of `keep` rows as real classifications. -->
    <!-- #6929: a truncated pass is a 200 with every worktree listed, but the
         rows it ran out of time on read `review` / `unknown-branch-state`. Say
         so, or an operator reads a partial answer as the whole one. -->
    {#if survey.partial}
      <div class="banner warning" role="status">
        <strong>Partial survey.</strong>
        The classification budget ran out before every worktree was inspected.
        Rows marked <em>review</em> with an unknown branch state were listed but
        not classified, and any missing size is unmeasured rather than zero.
      </div>
    {/if}
    {#if keepList?.error}
      <div class="banner danger" role="alert">
        <strong>Keep-list unreadable.</strong>
        {keepList.error} — every worktree is being held, so no row below is a
        reclaim judgement.
      </div>
    {/if}
    {#if keepList?.invalid?.length}
      <div class="banner warning">
        <strong>{keepList.invalid.length} keep-list pattern(s) ignored:</strong>
        <ul>
          {#each keepList.invalid as pattern, i (i)}
            <li><code>{pattern}</code></li>
          {/each}
        </ul>
      </div>
    {/if}

    <div class="summary">
      <span class="total">{formatBytes(survey.root?.bytes)}</span>
      <span class="path"><code>{survey.root?.path}</code></span>
      {#if counts}
        <span class="tallies">
          {#each TIERS as tier (tier)}
            <Badge tone={tierTone(tier)}>{counts[tier] ?? 0} {tierLabel(tier)}</Badge>
          {/each}
        </span>
      {/if}
    </div>

    <!-- The legend is always visible, never hover-only (DOC-73 §16.3). -->
    <ul class="legend" aria-label="Staleness tiers">
      {#each TIERS as tier (tier)}
        <li>
          <span class="swatch" style="background: {tierColor(tier)}"></span>{tierLabel(tier)}
        </li>
      {/each}
      <li><span class="swatch neutral"></span>Project</li>
    </ul>

    <div class="layout" class:narrow>
      {#if !narrow && rings && !rings.empty}
        <DiskSunburst {rings} selectedId={selected?.id ?? null} onSelect={(s) => (selected = s)} />
      {:else}
        <!-- Below 600px a sunburst's labels and touch targets stop working, so
             the same rows render as a sorted list with the same tier colours. -->
        <table class="rows">
          <thead>
            <tr>
              <th><button type="button" onclick={() => sortBy('name')}>Worktree</button></th>
              <th><button type="button" onclick={() => sortBy('tier')}>Tier</button></th>
              <th class="num">
                <button type="button" onclick={() => sortBy('bytes')}>Size</button>
              </th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row (row.kind + row.id)}
              {#if row.kind === 'project'}
                <tr class="project-row">
                  <th colspan="2" scope="rowgroup">{row.name}</th>
                  <td class="num">{formatBytes(row.bytes)}</td>
                </tr>
              {:else}
                <tr class:selected={selected?.id === row.id}>
                  <td>
                    <button type="button" class="row-open" onclick={() => selectRow(row)}>
                      {row.name}
                    </button>
                  </td>
                  <td><Badge tone={tierTone(row.tier)}>{tierLabel(row.tier)}</Badge></td>
                  <td class="num">{formatBytes(row.bytes)}</td>
                </tr>
              {/if}
            {/each}
          </tbody>
        </table>
      {/if}

      <DiskDetail segment={selected} />
    </div>
  {/if}
</div>

<style>
  .tab-content {
    min-width: 0;
  }
  .placeholder,
  .not-available {
    padding: 1.5rem;
    text-align: center;
    color: var(--trusty-text-secondary);
    border: 1px dashed var(--trusty-border);
    border-radius: 0.5rem;
  }
  .banner {
    border: 1px solid var(--trusty-border);
    border-left-width: 3px;
    border-radius: 0.4rem;
    padding: 0.55rem 0.8rem;
    margin-bottom: 0.75rem;
    font-size: 0.78rem;
    color: var(--trusty-text-primary);
    background: var(--trusty-card-bg);
  }
  .banner.danger {
    border-left-color: var(--trusty-danger);
  }
  .banner.warning {
    border-left-color: var(--trusty-warning);
  }
  .banner ul {
    margin: 0.3rem 0 0;
    padding-left: 1.1rem;
  }
  .summary {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 0.6rem;
    margin-bottom: 0.6rem;
  }
  .total {
    font-size: 1.2rem;
    font-weight: 600;
    font-family: var(--trusty-mono, monospace);
    color: var(--trusty-text-primary);
  }
  .path {
    color: var(--trusty-text-muted);
    font-size: 0.75rem;
  }
  .tallies {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
    margin-left: auto;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem 0.9rem;
    list-style: none;
    margin: 0 0 0.9rem;
    padding: 0;
    font-size: 0.73rem;
    color: var(--trusty-text-secondary);
  }
  .legend li {
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }
  .swatch {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 2px;
    display: inline-block;
  }
  .swatch.neutral {
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-border-strong);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 20rem);
    gap: 1rem;
    align-items: start;
  }
  .layout.narrow {
    grid-template-columns: minmax(0, 1fr);
  }
  .rows {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.78rem;
  }
  .rows th,
  .rows td {
    text-align: left;
    padding: 0.3rem 0.5rem;
    border-bottom: 1px solid var(--trusty-border);
    color: var(--trusty-text-primary);
  }
  .rows .num {
    text-align: right;
    font-family: var(--trusty-mono, monospace);
  }
  .rows thead button {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    font-weight: 600;
    color: var(--trusty-text-secondary);
    cursor: pointer;
  }
  .project-row th {
    color: var(--trusty-text-secondary);
    font-family: var(--trusty-mono, monospace);
    font-size: 0.72rem;
    padding-top: 0.7rem;
  }
  .row-open {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--trusty-accent);
    cursor: pointer;
    overflow-wrap: anywhere;
    text-align: left;
  }
  tr.selected {
    background: color-mix(in srgb, var(--trusty-accent) 8%, transparent);
  }
</style>
