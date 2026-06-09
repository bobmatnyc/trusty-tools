<script>
  /*
   * Why: The Indexes view is the admin pane for the daemon — list every
   * registered collection, register a new one, kick off a reindex, or evict
   * a stale one. The redesign (issue #38) adds a last-indexed column, a
   * per-index disk-usage column, and a live reindex-in-progress spinner fed
   * by the per-index `/reindex/stream` SSE feed.
   * What: Header card with a create form, then a table of every index with
   * disk + last-indexed columns and per-row Reindex / Delete buttons. A
   * reindex shows a spinner + progress until the SSE `complete` event.
   * Issue #682 adds multi-select checkboxes and a bulk-action toolbar so
   * operators can delete or reindex many indexes in one click.
   * Test: trigger a reindex on a seeded index and confirm the spinner shows
   * progress and clears on completion; select two rows and confirm bulk
   * Delete fires two DELETE calls and refreshes the list.
   */
  import { onDestroy } from 'svelte';
  import { api } from '../api.js';
  import { apiUrl } from '../base.js';
  import { getIndexes, getLoading, getError, refreshIndexes } from '../state.svelte.js';

  let indexes = $derived(getIndexes());
  let loading = $derived(getLoading());
  let error = $derived(getError());

  let newId = $state('');
  let newPath = $state('');
  let creating = $state(false);
  let createError = $state(null);
  let rowError = $state(null);
  let busyId = $state(null);

  // Per-index reindex progress: { [id]: { indexed, total } }. An entry's
  // presence means a reindex is in-flight; it is removed on completion.
  let progress = $state({});
  // Live EventSource handles keyed by index id so we can close on unmount.
  const streams = {};

  // -------------------------------------------------------------------------
  // Multi-select state (issue #682)
  // -------------------------------------------------------------------------

  /** Set of index ids currently selected via the per-row checkboxes. */
  let selected = $state(new Set());

  /** True when all (non-busy) rows are selected. */
  let allSelected = $derived(
    indexes.length > 0 && indexes.every((ix) => selected.has(ix.id))
  );

  /** Number of selected rows (drives the toolbar badge). */
  let selectedCount = $derived(selected.size);

  /**
   * Why: bulk-action state needs to show per-item feedback after fan-out.
   * What: array of { id, status: 'ok'|'error', message? } built during a
   * bulk operation; cleared on next bulk action or list refresh.
   * Test: mock deleteIndex to reject for one id; assert results contains
   * one 'error' entry and one 'ok' entry.
   */
  let bulkResults = $state([]);
  let bulkRunning = $state(false);
  /** Pending bulk op type – used to show a confirm dialog for Delete. */
  let pendingBulkOp = $state(null); // null | 'delete' | 'reindex'

  function toggleSelectAll() {
    if (allSelected) {
      selected = new Set();
    } else {
      selected = new Set(indexes.map((ix) => ix.id));
    }
  }

  function toggleRow(id) {
    const next = new Set(selected);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    selected = next;
  }

  // -------------------------------------------------------------------------

  onDestroy(() => {
    for (const id of Object.keys(streams)) closeStream(id);
  });

  function closeStream(id) {
    if (streams[id]) {
      streams[id].close();
      delete streams[id];
    }
  }

  async function createIndex(e) {
    e?.preventDefault?.();
    if (!newId.trim() || !newPath.trim()) return;
    creating = true;
    createError = null;
    try {
      await api.createIndex(newId.trim(), newPath.trim());
      newId = '';
      newPath = '';
      await refreshIndexes();
    } catch (err) {
      createError = err.message || String(err);
    } finally {
      creating = false;
    }
  }

  /**
   * Why: a reindex is fire-and-forget; the daemon returns an SSE stream URL.
   * Subscribing lets the table show live progress instead of a static label.
   * What: POSTs the reindex, then opens the `/reindex/stream` EventSource and
   * mirrors `progress`/`complete`/`error` events into local state.
   * Test: trigger a reindex, observe the spinner update then clear.
   */
  async function reindex(id) {
    rowError = null;
    busyId = id;
    try {
      const res = await api.reindex(id);
      progress = { ...progress, [id]: { indexed: 0, total: 0 } };
      const rawStreamUrl = res?.stream_url || `/indexes/${encodeURIComponent(id)}/reindex/stream`;
      // Guard: if the daemon returned an absolute URL (http/https), use it as-is;
      // otherwise rebase it through apiUrl so it resolves under the proxy sub-path.
      const url = /^https?:\/\//.test(rawStreamUrl) ? rawStreamUrl : apiUrl(rawStreamUrl);
      closeStream(id);
      const src = new EventSource(url);
      streams[id] = src;
      src.onmessage = (ev) => {
        let evt;
        try {
          evt = JSON.parse(ev.data);
        } catch {
          return;
        }
        if (evt.event === 'progress') {
          progress = {
            ...progress,
            [id]: { indexed: evt.indexed ?? 0, total: evt.total ?? 0 }
          };
        } else if (evt.event === 'complete' || evt.event === 'error') {
          const next = { ...progress };
          delete next[id];
          progress = next;
          closeStream(id);
          if (evt.event === 'error') {
            rowError = `Reindex ${id}: ${evt.message || 'failed'}`;
          }
          refreshIndexes().catch(() => {});
        }
      };
      src.onerror = () => {
        // Stream ended (daemon closes it on completion) — drop progress.
        const next = { ...progress };
        delete next[id];
        progress = next;
        closeStream(id);
      };
    } catch (err) {
      rowError = `Reindex ${id}: ${err.message || err}`;
    } finally {
      busyId = null;
    }
  }

  async function remove(id) {
    if (!confirm(`Delete index "${id}"? On-disk data is preserved.`)) return;
    rowError = null;
    busyId = id;
    try {
      closeStream(id);
      await api.deleteIndex(id);
      await refreshIndexes();
    } catch (err) {
      rowError = `Delete ${id}: ${err.message || err}`;
    } finally {
      busyId = null;
    }
  }

  // -------------------------------------------------------------------------
  // Bulk operations (issue #682)
  // -------------------------------------------------------------------------

  /** Max concurrent requests for bulk fan-out (avoids overwhelming the daemon). */
  const BULK_CONCURRENCY = 4;

  /**
   * Why: Bulk delete/reindex fans out N API calls; running them all in parallel
   * can overwhelm a single-daemon process. This helper runs `tasks` with at most
   * `concurrency` in-flight at once, exactly like a semaphore.
   * What: splits the task list into windows of `concurrency`, awaits each window
   * before starting the next. Returns an ordered array of settled results.
   * Test: pass 6 tasks with concurrency=2, assert each batch of 2 completes
   * before the next starts.
   */
  async function pLimit(tasks, concurrency) {
    const results = [];
    for (let i = 0; i < tasks.length; i += concurrency) {
      const batch = tasks.slice(i, i + concurrency);
      const settled = await Promise.allSettled(batch.map((fn) => fn()));
      results.push(...settled);
    }
    return results;
  }

  /**
   * Why: operators may want to initiate a bulk op but accidentally trigger it;
   * surfacing a confirm step for Delete (destructive) prevents data loss.
   * What: sets `pendingBulkOp` to show an inline confirm toolbar; the actual
   * fan-out runs from `confirmBulkOp`.
   * Test: click "Delete selected", assert confirm banner appears; click Cancel,
   * assert no DELETE calls were made.
   */
  function startBulkOp(op) {
    if (op === 'delete') {
      pendingBulkOp = 'delete';
    } else {
      // Reindex has no destructive data loss — execute immediately.
      executeBulkReindex();
    }
  }

  function cancelBulkOp() {
    pendingBulkOp = null;
  }

  async function confirmBulkOp() {
    if (pendingBulkOp === 'delete') {
      pendingBulkOp = null;
      await executeBulkDelete();
    }
  }

  /**
   * Why: bulk delete must run with bounded concurrency and surface per-item
   * success/failure so operators can see which indexes failed.
   * What: iterates the selected set, calls api.deleteIndex for each, collects
   * results, refreshes the list, and clears the selection.
   * Test: select 3 indexes, bulk delete; assert all three rows disappear and
   * bulkResults has 3 entries each with status='ok'.
   */
  async function executeBulkDelete() {
    const ids = [...selected];
    bulkRunning = true;
    bulkResults = [];
    rowError = null;
    try {
      const tasks = ids.map((id) => async () => {
        closeStream(id);
        try {
          await api.deleteIndex(id);
          return { id, status: 'ok' };
        } catch (err) {
          return { id, status: 'error', message: err.message || String(err) };
        }
      });
      const settled = await pLimit(tasks, BULK_CONCURRENCY);
      bulkResults = settled.map((r) =>
        r.status === 'fulfilled'
          ? r.value
          : { id: '?', status: 'error', message: r.reason?.message || String(r.reason) }
      );
      const failed = bulkResults.filter((r) => r.status === 'error');
      if (failed.length > 0) {
        rowError = `Bulk delete: ${failed.length} failed — ${failed.map((f) => f.id).join(', ')}`;
      }
    } finally {
      bulkRunning = false;
      selected = new Set();
      await refreshIndexes().catch(() => {});
    }
  }

  /**
   * Why: bulk reindex queues N reindex jobs concurrently; SSE progress is per-
   * index so existing single-row streaming still works for each.
   * What: calls the existing `reindex(id)` function for every selected id in
   * bounded batches, reusing the SSE progress wiring already in place.
   * Test: select 2 indexes, bulk reindex; assert both rows show spinners.
   */
  async function executeBulkReindex() {
    const ids = [...selected];
    bulkRunning = true;
    bulkResults = [];
    rowError = null;
    try {
      const tasks = ids.map((id) => async () => {
        try {
          // Reuse the single-row reindex which wires SSE progress for each id.
          await reindex(id);
          return { id, status: 'ok' };
        } catch (err) {
          return { id, status: 'error', message: err.message || String(err) };
        }
      });
      const settled = await pLimit(tasks, BULK_CONCURRENCY);
      bulkResults = settled.map((r) =>
        r.status === 'fulfilled'
          ? r.value
          : { id: '?', status: 'error', message: r.reason?.message || String(r.reason) }
      );
      const failed = bulkResults.filter((r) => r.status === 'error');
      if (failed.length > 0) {
        rowError = `Bulk reindex: ${failed.length} failed — ${failed.map((f) => f.id).join(', ')}`;
      }
    } finally {
      bulkRunning = false;
      selected = new Set();
    }
  }

  // -------------------------------------------------------------------------

  /**
   * Why: disk_bytes is raw; operators want human units.
   * What: byte-count → "X KB / MB / GB", or "—" when null.
   * Test: humanBytes(1048576) === "1.0 MB".
   */
  function humanBytes(bytes) {
    if (typeof bytes !== 'number') return '—';
    if (bytes < 1024) return `${bytes} B`;
    const kb = bytes / 1024;
    if (kb < 1024) return `${kb.toFixed(1)} KB`;
    const mb = kb / 1024;
    if (mb < 1024) return `${mb.toFixed(1)} MB`;
    return `${(mb / 1024).toFixed(2)} GB`;
  }

  /**
   * Why: an RFC3339 timestamp is precise but not glanceable.
   * What: render as a localised date-time, or "never" when null.
   * Test: humanTime(null) === "never".
   */
  function humanTime(iso) {
    if (!iso) return 'never';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }
</script>

<h1 class="page-title">Indexes</h1>

<div class="card mb-4">
  <div class="card-header">Register a new index</div>
  <div class="card-body">
    <form class="create-row" onsubmit={createIndex}>
      <input
        type="text"
        class="input"
        placeholder="Index id (e.g. my-project)"
        bind:value={newId}
      />
      <input
        type="text"
        class="input"
        placeholder="Absolute root path (e.g. /Users/me/code/my-project)"
        bind:value={newPath}
      />
      <button
        type="submit"
        class="btn btn-primary"
        disabled={creating || !newId.trim() || !newPath.trim()}
      >
        {creating ? 'Creating…' : 'Create'}
      </button>
    </form>
    {#if createError}
      <p class="text-sm mt-3" style="color: var(--trusty-danger)">{createError}</p>
    {/if}
  </div>
</div>

<div class="card">
  <div class="card-header flex-between">
    <span>Registered indexes</span>
    <button class="btn btn-sm" onclick={refreshIndexes} disabled={loading}>
      {loading ? 'Refreshing…' : 'Refresh'}
    </button>
  </div>

  <!-- Bulk-action toolbar (issue #682): shown when ≥1 row is selected -->
  {#if selectedCount > 0}
    <div class="bulk-toolbar">
      {#if pendingBulkOp === 'delete'}
        <!-- Confirm step for destructive delete -->
        <span class="bulk-confirm-text">
          Delete {selectedCount} index{selectedCount === 1 ? '' : 'es'}? On-disk data is preserved.
        </span>
        <button
          class="btn btn-sm btn-danger"
          disabled={bulkRunning}
          onclick={confirmBulkOp}
        >
          {bulkRunning ? 'Deleting…' : 'Confirm delete'}
        </button>
        <button class="btn btn-sm" disabled={bulkRunning} onclick={cancelBulkOp}>
          Cancel
        </button>
      {:else}
        <span class="bulk-count">{selectedCount} selected</span>
        <button
          class="btn btn-sm"
          disabled={bulkRunning}
          onclick={() => startBulkOp('reindex')}
        >
          {bulkRunning ? 'Working…' : 'Reindex selected'}
        </button>
        <button
          class="btn btn-sm btn-danger"
          disabled={bulkRunning}
          onclick={() => startBulkOp('delete')}
        >
          Delete selected
        </button>
        <button
          class="btn btn-sm btn-ghost"
          disabled={bulkRunning}
          onclick={() => (selected = new Set())}
        >
          Clear selection
        </button>
      {/if}
    </div>
  {/if}

  <div class="card-body" style="padding: 0">
    {#if rowError}
      <div class="row-error">{rowError}</div>
    {/if}
    {#if error}
      <div class="row-error">{error}</div>
    {/if}
    {#if indexes.length === 0}
      <div class="empty">
        {#if loading}Loading…{:else}No indexes registered yet.{/if}
      </div>
    {:else}
      <div class="table-wrap">
        <table class="table">
          <thead>
            <tr>
              <!-- Select-all header checkbox (issue #682) -->
              <th class="col-check">
                <input
                  type="checkbox"
                  class="checkbox"
                  checked={allSelected}
                  indeterminate={selectedCount > 0 && !allSelected}
                  onchange={toggleSelectAll}
                  aria-label="Select all indexes"
                />
              </th>
              <th>Name</th>
              <th>Documents</th>
              <th>Disk</th>
              <th>Last indexed</th>
              <th>Root path</th>
              <th>Status</th>
              <th style="text-align: right">Actions</th>
            </tr>
          </thead>
          <tbody>
            {#each indexes as ix (ix.id)}
              <tr class:selected-row={selected.has(ix.id)}>
                <!-- Per-row checkbox (issue #682) -->
                <td class="col-check">
                  <input
                    type="checkbox"
                    class="checkbox"
                    checked={selected.has(ix.id)}
                    onchange={() => toggleRow(ix.id)}
                    aria-label={`Select ${ix.id}`}
                  />
                </td>
                <td><strong>{ix.id}</strong></td>
                <td>{(ix.chunk_count ?? 0).toLocaleString()}</td>
                <td class="text-mono text-xs">{humanBytes(ix.disk_bytes)}</td>
                <td class="text-xs text-muted">{humanTime(ix.last_indexed)}</td>
                <td class="text-mono text-xs text-muted truncate" style="max-width: 280px">
                  {ix.root_path || '—'}
                </td>
                <td>
                  {#if progress[ix.id]}
                    <span class="badge badge-info reindex-badge">
                      <span class="spinner"></span>
                      {#if progress[ix.id].total > 0}
                        {progress[ix.id].indexed}/{progress[ix.id].total}
                      {:else}
                        reindexing…
                      {/if}
                    </span>
                  {:else if ix.error}
                    <span class="badge badge-danger">error</span>
                  {:else}
                    <span class="badge badge-success">ready</span>
                  {/if}
                </td>
                <td style="text-align: right; white-space: nowrap">
                  <button
                    class="btn btn-sm"
                    disabled={busyId === ix.id || !!progress[ix.id]}
                    onclick={() => reindex(ix.id)}
                  >
                    {progress[ix.id] ? 'Working…' : 'Reindex'}
                  </button>
                  <button
                    class="btn btn-sm btn-danger"
                    disabled={busyId === ix.id}
                    onclick={() => remove(ix.id)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .create-row {
    display: grid;
    grid-template-columns: 1fr 2fr auto;
    gap: var(--trusty-space-2);
    align-items: stretch;
  }
  .table-wrap {
    overflow-x: auto;
  }
  @media (max-width: 720px) {
    .create-row {
      grid-template-columns: 1fr;
    }
  }
  .row-error {
    padding: var(--trusty-space-3) var(--trusty-space-5);
    color: var(--trusty-danger);
    background: var(--trusty-danger-soft);
    border-bottom: 1px solid var(--trusty-border);
    font-size: var(--trusty-fs-sm);
  }
  .reindex-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .spinner {
    width: 10px;
    height: 10px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    display: inline-block;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* Bulk-action toolbar (issue #682) */
  .bulk-toolbar {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    padding: var(--trusty-space-2) var(--trusty-space-4);
    background: var(--trusty-surface-raised, #f5f5f5);
    border-bottom: 1px solid var(--trusty-border);
    flex-wrap: wrap;
  }
  .bulk-count {
    font-size: var(--trusty-fs-sm);
    font-weight: 600;
    color: var(--trusty-text);
    margin-right: var(--trusty-space-1);
  }
  .bulk-confirm-text {
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-danger);
    font-weight: 500;
    margin-right: var(--trusty-space-1);
  }
  .btn-ghost {
    background: transparent;
    border-color: transparent;
    color: var(--trusty-text-muted);
  }
  .btn-ghost:hover:not(:disabled) {
    background: var(--trusty-surface-hover, rgba(0, 0, 0, 0.06));
    color: var(--trusty-text);
  }

  /* Per-row checkbox column */
  .col-check {
    width: 36px;
    padding-left: var(--trusty-space-3);
    padding-right: 0;
  }
  .checkbox {
    cursor: pointer;
    width: 15px;
    height: 15px;
    accent-color: var(--trusty-primary, #3b82f6);
  }

  /* Highlight selected rows */
  .selected-row {
    background: var(--trusty-primary-soft, rgba(59, 130, 246, 0.07));
  }
</style>
