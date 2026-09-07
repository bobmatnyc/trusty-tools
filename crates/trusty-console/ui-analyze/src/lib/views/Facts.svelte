<script>
  /*
   * Why: FactStore is the (subject, predicate, object) knowledge layer; the
   * UI lets operators search and review facts without leaving the dashboard.
   * What: Search inputs for subject/predicate that drive /facts queries, plus
   * a results table.
   * Test: Type a subject, click Search, confirm only matching rows return.
   */
  import { onMount } from 'svelte';
  import { api } from '../api.js';
  import { getFacts, refreshFacts } from '../state.svelte.js';

  let facts = $derived(getFacts());
  let subject = $state('');
  let predicate = $state('');
  let error = $state(null);
  let busy = $state(false);

  onMount(() => {
    refreshFacts().catch((e) => {
      error = e.message || String(e);
    });
  });

  async function search() {
    error = null;
    busy = true;
    try {
      await refreshFacts(subject || undefined, predicate || undefined);
    } catch (e) {
      error = e.message || String(e);
    } finally {
      busy = false;
    }
  }

  async function remove(id) {
    if (!id) return;
    try {
      await api.deleteFact(id);
      await refreshFacts(subject || undefined, predicate || undefined);
    } catch (e) {
      error = e.message || String(e);
    }
  }
</script>

<h1 class="page-title">Facts</h1>

<div class="card mb-4">
  <div class="card-body">
    <div class="search-row">
      <label class="form-group" style="flex: 1; margin: 0">
        <div class="form-label">Subject</div>
        <input class="input" type="text" bind:value={subject} placeholder="fn auth" />
      </label>
      <label class="form-group" style="flex: 1; margin: 0">
        <div class="form-label">Predicate</div>
        <input class="input" type="text" bind:value={predicate} placeholder="uses" />
      </label>
      <button class="btn btn-primary" onclick={search} disabled={busy}>
        {busy ? 'Searching…' : 'Search'}
      </button>
    </div>
    {#if error}
      <p class="text-sm mt-3" style="color: var(--trusty-danger)">{error}</p>
    {/if}
  </div>
</div>

<div class="card">
  <div class="card-header">Results ({facts.length})</div>
  <div class="card-body" style="padding: 0">
    {#if facts.length === 0}
      <div class="empty">No facts.</div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Subject</th>
            <th>Predicate</th>
            <th>Object</th>
            <th>Provenance</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each facts as f}
            <tr>
              <td class="text-mono text-xs">{f.subject}</td>
              <td class="text-mono text-xs">{f.predicate}</td>
              <td class="text-mono text-xs">{f.object}</td>
              <td class="text-muted text-xs">{f.provenance || '—'}</td>
              <td>
                {#if f.id}
                  <button class="btn btn-sm btn-danger" onclick={() => remove(f.id)}>
                    delete
                  </button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .search-row {
    display: flex;
    gap: var(--trusty-space-3);
    align-items: flex-end;
  }
</style>
