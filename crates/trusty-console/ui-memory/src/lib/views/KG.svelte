<script>
  /*
   * Why: Operators want to browse the knowledge graph without knowing
   * subjects in advance. The existing `GET /api/v1/palaces/{id}/kg`
   * endpoint requires a subject; this view layers a UI on top of the new
   * `/kg/subjects` and `/kg/all` endpoints so the SPA can paginate or drill
   * into a specific subject's triples.
   * What: Palace dropdown + left subject panel + right triple table. "All"
   * mode pages through every active triple. Selecting a subject loads its
   * triples via the existing endpoint.
   * Test: open #/kg, pick a palace, confirm subjects load on the left and
   * clicking one populates the right table.
   */
  import { onMount } from 'svelte';
  import { api } from '../api.js';

  const PAGE_SIZE = 50;

  let palaces = $state([]);
  let palaceId = $state('');
  let mode = $state('all'); // 'all' | 'subject'
  let selectedSubject = $state('');
  // subjects is now [{subject, count}] so the panel can show a count badge
  // and sort by count without extra round-trips.
  let subjects = $state([]);
  let triples = $state([]);
  let count = $state(null);
  let offset = $state(0);
  let loading = $state(false);
  let error = $state(null);

  // Subject panel filter + sort state.
  let subjectFilter = $state('');
  let subjectSort = $state('name'); // 'name' | 'count'

  async function refreshPalaces() {
    try {
      palaces = await api.listPalaces();
      if (!palaceId && palaces.length > 0) {
        palaceId = palaces[0].id;
        await onPalaceChange();
      }
    } catch (e) {
      error = e.message || String(e);
    }
  }

  async function loadSubjects() {
    if (!palaceId) {
      subjects = [];
      return;
    }
    try {
      const rows = await api.kgListSubjectsWithCounts(palaceId, 200);
      // Server returns [{subject, count}]; tolerate legacy [subject] shape so
      // a stale daemon still populates the panel (with count=0).
      subjects = (rows || []).map((r) =>
        typeof r === 'string'
          ? { subject: r, count: 0 }
          : { subject: r.subject ?? '', count: Number(r.count ?? 0) }
      );
    } catch (e) {
      error = e.message || String(e);
      subjects = [];
    }
  }

  /**
   * Why: 200 subjects is a lot; operators want a substring filter to find
   * the one they care about, plus a sort toggle (alphabetical vs by triple
   * count descending).
   * What: applies substring + sort to the loaded subjects list.
   * Test: visibleSubjects with filter 'al' on [{subject:'alice'},{subject:'bob'}]
   * yields just 'alice'.
   */
  let visibleSubjects = $derived.by(() => {
    const f = subjectFilter.trim().toLowerCase();
    let out = subjects.slice();
    if (f) {
      out = out.filter((s) => (s.subject || '').toLowerCase().includes(f));
    }
    if (subjectSort === 'count') {
      out.sort(
        (a, b) =>
          (b.count ?? 0) - (a.count ?? 0) ||
          (a.subject || '').localeCompare(b.subject || '')
      );
    } else {
      out.sort((a, b) => (a.subject || '').localeCompare(b.subject || ''));
    }
    return out;
  });

  async function loadAll() {
    if (!palaceId) {
      triples = [];
      return;
    }
    loading = true;
    try {
      triples = await api.kgListAll(palaceId, { limit: PAGE_SIZE, offset });
      error = null;
    } catch (e) {
      error = e.message || String(e);
      triples = [];
    } finally {
      loading = false;
    }
  }

  async function loadSubject(subj) {
    if (!palaceId || !subj) return;
    selectedSubject = subj;
    mode = 'subject';
    loading = true;
    try {
      triples = await api.kgQuery(palaceId, subj);
      error = null;
    } catch (e) {
      error = e.message || String(e);
      triples = [];
    } finally {
      loading = false;
    }
  }

  async function loadCount() {
    if (!palaceId) {
      count = null;
      return;
    }
    try {
      const r = await api.kgCount(palaceId);
      count = r?.active ?? null;
    } catch {
      count = null;
    }
  }

  async function onPalaceChange() {
    offset = 0;
    selectedSubject = '';
    mode = 'all';
    await Promise.all([loadSubjects(), loadAll(), loadCount()]);
  }

  function showAll() {
    mode = 'all';
    selectedSubject = '';
    offset = 0;
    loadAll();
  }

  function prevPage() {
    if (offset === 0) return;
    offset = Math.max(0, offset - PAGE_SIZE);
    loadAll();
  }

  function nextPage() {
    if (triples.length < PAGE_SIZE) return;
    offset += PAGE_SIZE;
    loadAll();
  }

  /**
   * Why: `valid_from` is an RFC3339 string; operators want a localised glance.
   * What: returns a short local time string, or em-dash when absent/invalid.
   * Test: humanTime("2026-01-01T00:00:00Z") yields a non-empty string.
   */
  function humanTime(iso) {
    if (!iso) return '—';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

  onMount(() => {
    refreshPalaces();
  });
</script>

<div class="page-head">
  <h1 class="page-title">KG Explorer</h1>
  <div class="head-meta">
    {#if count !== null}
      <span class="badge">{count.toLocaleString()} active triples</span>
    {/if}
  </div>
</div>

<div class="card">
  <div class="card-body controls">
    <label class="ctl">
      <span class="lbl">Palace</span>
      <select bind:value={palaceId} onchange={onPalaceChange}>
        {#if palaces.length === 0}
          <option value="">(no palaces)</option>
        {/if}
        {#each palaces as p}
          <option value={p.id}>{p.name} ({p.id})</option>
        {/each}
      </select>
    </label>
    <button type="button" class="btn" class:active={mode === 'all'} onclick={showAll}>
      All triples
    </button>
    {#if mode === 'subject' && selectedSubject}
      <span class="subj-pill">
        subject: <strong>{selectedSubject}</strong>
      </span>
    {/if}
  </div>
</div>

{#if error}
  <div class="card mt-3" style="border-color: var(--trusty-danger, #ef4444)">
    <div class="card-body" style="color: var(--trusty-danger, #ef4444)">
      {error}
    </div>
  </div>
{/if}

<div class="explorer mt-4">
  <aside class="left">
    <div class="panel-head">
      <span>Subjects ({visibleSubjects.length}/{subjects.length})</span>
    </div>
    <div class="subj-controls">
      <input
        type="search"
        class="subj-filter"
        placeholder="🔍 Filter subjects…"
        bind:value={subjectFilter}
      />
      <label class="subj-sort">
        <span>Sort</span>
        <select bind:value={subjectSort}>
          <option value="name">A→Z</option>
          <option value="count">Count</option>
        </select>
      </label>
    </div>
    <div class="panel-body">
      {#if visibleSubjects.length === 0}
        <div class="empty">
          {subjects.length === 0 ? 'No subjects.' : 'No matches.'}
        </div>
      {:else}
        <ul class="subj-list">
          {#each visibleSubjects as s (s.subject)}
            <li>
              <button
                type="button"
                class="subj-btn"
                class:active={selectedSubject === s.subject}
                onclick={() => loadSubject(s.subject)}
              >
                <span class="subj-name">{s.subject}</span>
                <span class="subj-count">{s.count}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </aside>

  <section class="right">
    <div class="panel-head">
      {#if mode === 'all'}
        All triples
      {:else}
        Triples for "{selectedSubject}"
      {/if}
      <span class="text-muted text-xs">
        {#if loading}loading…{:else}{triples.length} rows{/if}
      </span>
    </div>
    <div class="panel-body" style="padding: 0">
      {#if triples.length === 0 && !loading}
        <div class="empty">No triples.</div>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th>Subject</th>
              <th>Predicate</th>
              <th>Object</th>
              <th style="width: 80px">Conf.</th>
              <th style="width: 180px">Valid From</th>
              <th>Provenance</th>
            </tr>
          </thead>
          <tbody>
            {#each triples as t}
              <tr>
                <td class="text-mono text-xs">{t.subject}</td>
                <td class="text-mono text-xs">{t.predicate}</td>
                <td>{t.object}</td>
                <td>{(t.confidence ?? 0).toFixed(2)}</td>
                <td class="text-xs text-muted">{humanTime(t.valid_from)}</td>
                <td class="text-xs text-muted">{t.provenance ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>

    {#if mode === 'all'}
      <div class="pager">
        <button type="button" class="btn" disabled={offset === 0} onclick={prevPage}>
          ← Prev
        </button>
        <span class="text-xs text-muted">
          offset {offset.toLocaleString()} – {(offset + triples.length).toLocaleString()}
        </span>
        <button
          type="button"
          class="btn"
          disabled={triples.length < PAGE_SIZE}
          onclick={nextPage}
        >
          Next →
        </button>
      </div>
    {/if}
  </section>
</div>

<style>
  .page-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--trusty-space-5, 16px);
    flex-wrap: wrap;
    gap: var(--trusty-space-3, 8px);
  }
  .page-title {
    font-size: var(--trusty-fs-xl, 20px);
    margin: 0;
    font-weight: 600;
  }
  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 4px;
    background: var(--trusty-surface-raised, #f3f4f6);
    font-size: 11px;
    color: var(--trusty-text-secondary, #6b7280);
  }
  .controls {
    display: flex;
    gap: 12px;
    align-items: center;
    flex-wrap: wrap;
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .lbl {
    font-size: 12px;
    color: var(--trusty-text-secondary, #6b7280);
  }
  select {
    padding: 4px 8px;
    border-radius: 4px;
    border: 1px solid var(--trusty-border, #e5e7eb);
    background: white;
    font-size: 13px;
  }
  .btn {
    padding: 4px 10px;
    border-radius: 4px;
    border: 1px solid var(--trusty-border, #e5e7eb);
    background: white;
    cursor: pointer;
    font-size: 12px;
  }
  .btn:hover:not(:disabled) {
    background: var(--trusty-surface-raised, #f9fafb);
  }
  .btn.active {
    background: var(--trusty-accent, #4f46e5);
    color: white;
    border-color: var(--trusty-accent, #4f46e5);
  }
  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .subj-pill {
    font-size: 12px;
    color: var(--trusty-text-secondary, #6b7280);
  }

  .explorer {
    display: grid;
    grid-template-columns: minmax(220px, 30%) 1fr;
    gap: 16px;
    align-items: start;
  }
  .left,
  .right {
    background: var(--trusty-content-bg, white);
    border: 1px solid var(--trusty-border, #e5e7eb);
    border-radius: 6px;
    overflow: hidden;
  }
  .panel-head {
    padding: 10px 14px;
    background: var(--trusty-surface-raised, #fafafa);
    border-bottom: 1px solid var(--trusty-border, #e5e7eb);
    font-size: 13px;
    font-weight: 600;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .panel-body {
    padding: 8px 0;
    max-height: 600px;
    overflow-y: auto;
  }
  .empty {
    text-align: center;
    color: var(--trusty-text-muted, #9ca3af);
    font-size: 12px;
    padding: 24px 16px;
  }
  .subj-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .subj-btn {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    width: 100%;
    text-align: left;
    padding: 6px 14px;
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 13px;
    color: var(--trusty-text-primary, #111827);
    font-family: var(--trusty-mono, monospace);
  }
  .subj-btn:hover {
    background: var(--trusty-surface-raised, #f3f4f6);
  }
  .subj-btn.active {
    background: var(--trusty-accent-soft, #e0e7ff);
    color: var(--trusty-accent, #4f46e5);
    font-weight: 600;
  }
  .subj-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
  .subj-count {
    flex: 0 0 auto;
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 8px;
    background: var(--trusty-surface-raised, #f3f4f6);
    color: var(--trusty-text-secondary, #6b7280);
    font-family: var(--trusty-font, sans-serif);
  }
  .subj-btn.active .subj-count {
    background: var(--trusty-accent, #4f46e5);
    color: white;
  }
  .subj-controls {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--trusty-border, #e5e7eb);
    background: var(--trusty-surface-raised, #fafafa);
  }
  .subj-filter {
    width: 100%;
    padding: 4px 8px;
    border-radius: 4px;
    border: 1px solid var(--trusty-border, #e5e7eb);
    font-size: 12px;
  }
  .subj-sort {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--trusty-text-secondary, #6b7280);
  }
  .subj-sort select {
    flex: 1;
    padding: 2px 6px;
    font-size: 11px;
  }

  .table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  .table th,
  .table td {
    padding: 6px 10px;
    text-align: left;
    border-bottom: 1px solid var(--trusty-border, #f3f4f6);
  }
  .table th {
    background: var(--trusty-surface-raised, #fafafa);
    font-size: 11px;
    color: var(--trusty-text-secondary, #6b7280);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 14px;
    border-top: 1px solid var(--trusty-border, #e5e7eb);
    background: var(--trusty-surface-raised, #fafafa);
  }
</style>
