<script>
  /*
   * Why: K-means concept clusters give a high-level "what is this codebase
   * about?" map; rendering each cluster as a card with its top centroid terms
   * is the most compact way to scan the conceptual landscape.
   * What: Card grid — one card per cluster — showing label, centroid term
   * chips, and chunk count. k is user-controllable; the embedding method is
   * always `bow` since #5067 removed the neural backend.
   * Test: Select an index that has chunks; expect at least 1 card with terms.
   */
  import { onMount } from 'svelte';
  import {
    getSelectedIndex,
    getClusters,
    refreshClusters
  } from '../state.svelte.js';

  let selected = $derived(getSelectedIndex());
  let clusters = $derived(getClusters());
  let k = $state(8);
  // #5067: the neural backend was removed; `bow` is the only method the API accepts.
  const method = 'bow';

  $effect(() => {
    if (!selected) return;
    refreshClusters(selected, { k, method }).catch(() => {});
  });

  onMount(() => {
    if (selected) refreshClusters(selected, { k, method }).catch(() => {});
  });
</script>

<h1 class="page-title">Concept Clusters</h1>

{#if !selected}
  <div class="card"><div class="empty">Select an index in the top bar.</div></div>
{:else}
  <div class="filter-bar">
    <label class="text-xs text-muted">
      k
      <input class="input" type="number" min="2" max="32" bind:value={k} style="margin-left: 8px; width: 90px" />
    </label>
  </div>

  {#if clusters.length === 0}
    <div class="card"><div class="empty">Loading clusters…</div></div>
  {:else}
    <div class="cluster-grid">
      {#each clusters as c, i}
        <div class="cluster-card">
          <div class="cl-head">
            <span class="cl-num">#{i + 1}</span>
            <strong class="cl-label">{c.label || `cluster ${i + 1}`}</strong>
          </div>
          <div class="cl-chunks text-xs text-muted">
            {(c.chunk_ids?.length ?? c.size ?? 0)} chunks
          </div>
          {#if c.centroid_terms?.length}
            <div class="cl-terms">
              {#each c.centroid_terms.slice(0, 12) as t}
                <span class="tag">{typeof t === 'string' ? t : (t.term || t.token || '?')}</span>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .filter-bar {
    display: flex;
    gap: var(--trusty-space-4);
    align-items: center;
    margin-bottom: var(--trusty-space-4);
  }
  .cluster-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: var(--trusty-space-4);
  }
  .cluster-card {
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: var(--trusty-space-4);
    box-shadow: var(--trusty-shadow-sm);
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
  }
  .cl-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .cl-num {
    font-family: var(--trusty-mono);
    color: var(--trusty-text-muted);
    font-size: var(--trusty-fs-xs);
  }
  .cl-label {
    font-size: var(--trusty-fs-md);
  }
  .cl-terms {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .cl-terms .tag {
    margin: 0;
    padding: 3px 8px;
    background: var(--trusty-accent-soft);
    color: var(--trusty-accent);
    border-radius: 999px;
  }
</style>
