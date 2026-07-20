<script>
  import AppShell from '../../lib/AppShell.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';
  import Modal from '../../lib/Modal.svelte';
  import Toast from '../../lib/Toast.svelte';
  import { searchNav, indexes } from './data.js';

  let rows = $state(indexes.map((ix) => ({ ...ix })));
  let showDelete = $state(true);
  const selected = $derived(rows.filter((r) => r.selected));
</script>

{#snippet actions()}
  <Button variant="danger" size="sm">STOP</Button>
  <Button size="sm">RESTART</Button>
{/snippet}

<AppShell sidebar={searchNav('Indexes')} crumb="INDEXES" topbarActions={actions}>
  <h1 class="page-title">INDEXES</h1>

  <div class="card register">
    <div class="card-header">REGISTER A NEW INDEX</div>
    <div class="reg-row">
      <input class="input" placeholder="index-id (e.g. my-project)">
      <input class="input" placeholder="/absolute/root/path">
      <Button disabled>CREATE</Button>
    </div>
  </div>

  <div class="card">
    <div class="card-header flex-between">
      <span>REGISTERED INDEXES</span>
      <Button size="sm">REFRESH</Button>
    </div>
    {#if selected.length}
      <div class="bulkbar">
        <span class="count">{selected.length} SELECTED</span>
        <Button size="sm">REINDEX SELECTED</Button>
        <Button variant="primary" size="sm" onclick={() => (showDelete = true)}>DELETE SELECTED</Button>
        <Button variant="ghost" size="sm" onclick={() => rows.forEach((r) => (r.selected = false))}>CLEAR</Button>
      </div>
    {/if}
    <table class="table">
      <thead>
        <tr>
          <th class="check"><input type="checkbox"></th>
          <th>Name</th><th>Docs</th><th>Disk</th><th>Last indexed</th><th>Root path</th><th>Status</th>
          <th class="right">Actions</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row}
          <tr class:selected-row={row.selected}>
            <td class="check"><input type="checkbox" bind:checked={row.selected}></td>
            <td class="name">{row.name}</td>
            <td class="text-mono">{row.docs}</td>
            <td class="mono-sm">{row.disk}</td>
            <td class="dim">{row.last}</td>
            <td class="path">{row.path}</td>
            <td>
              {#if row.status === 'working'}
                <Badge tone="warning" spinner>{row.progress}</Badge>
              {:else if row.status === 'error'}
                <Badge tone="danger">ERROR</Badge>
              {:else}
                <Badge tone="success">READY</Badge>
              {/if}
            </td>
            <td class="right acts">
              <Button size="sm">⚙ SETTINGS</Button>
              {#if row.status === 'working'}
                <Button size="sm" disabled>WORKING…</Button>
              {:else}
                <Button size="sm">REINDEX</Button>
              {/if}
              <Button variant="danger" size="sm">DELETE</Button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  {#if showDelete}
    <Modal title="DELETE 2 INDEXES?" onclose={() => (showDelete = false)}>
      <p>
        <code class="ref">trusty-tools</code> and <code class="ref">memory-palace</code>
        will be removed from the registry.
      </p>
      <p class="dim-p">On-disk data is preserved. You can re-register the same paths later.</p>
      {#snippet footer()}
        <Button onclick={() => (showDelete = false)}>CANCEL</Button>
        <Button variant="primary">CONFIRM DELETE</Button>
      {/snippet}
    </Modal>
  {/if}

  <div class="toast-stack">
    <Toast title="REINDEX STARTED">memory-palace — queued 31,870 chunks.</Toast>
    <Toast tone="danger" title="REINDEX FAILED">gitflow-rs — root path not found. <a href="#log">View log</a></Toast>
  </div>
</AppShell>

<style>
  h1 { font-size: 28px; margin: 0 0 24px; }
  .register { margin-bottom: 16px; }
  .reg-row { padding: 20px; display: grid; grid-template-columns: 1fr 2fr auto; gap: 8px; }
  .bulkbar { display: flex; align-items: center; gap: 8px; padding: 9px 20px; background: var(--trusty-accent-soft); border-bottom: 1.5px solid var(--trusty-border); }
  .count { font: 600 12px var(--trusty-mono); color: var(--trusty-accent-hover); margin-right: 6px; }
  .check { width: 38px; padding-right: 0; }
  .check input { width: 15px; height: 15px; }
  .right { text-align: right; }
  .name { font-weight: 600; }
  .mono-sm { font: 400 11px var(--trusty-mono); }
  .dim { font-size: 11px; color: var(--trusty-text-muted); }
  .path { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .acts { white-space: nowrap; }
  .acts :global(.btn + .btn) { margin-left: 4px; }
  .ref { font: 600 12px var(--trusty-mono); color: var(--trusty-accent); }
  p { margin: 0 0 10px; font-size: 13.5px; line-height: 1.6; color: var(--trusty-text-secondary); }
  .dim-p { margin: 0; color: var(--trusty-text-muted); }
</style>
