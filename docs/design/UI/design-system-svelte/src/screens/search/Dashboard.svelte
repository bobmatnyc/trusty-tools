<script>
  import AppShell from '../../lib/AppShell.svelte';
  import StatCard from '../../lib/StatCard.svelte';
  import Badge from '../../lib/Badge.svelte';
  import Button from '../../lib/Button.svelte';
  import Toast from '../../lib/Toast.svelte';
  import { searchNav, indexes } from './data.js';

  let toastVisible = $state(true);
  const statusBadge = { ready: ['success', 'READY'], error: ['danger', 'ERROR'] };
</script>

{#snippet actions()}
  <Button variant="danger" size="sm">STOP</Button>
  <Button size="sm">RESTART</Button>
{/snippet}

<AppShell sidebar={searchNav('Dashboard')} crumb="DASHBOARD" topbarActions={actions}>
  <h1 class="page-title">DASHBOARD</h1>
  <div class="stat-grid four">
    <StatCard label="INDEXES" value="4" meta="registered" />
    <StatCard label="DOCUMENTS" value="128,441" meta="indexed chunks" accent />
    <StatCard label="UPTIME" value="3d 04h" meta="daemon" />
    <StatCard label="VERSION">
      <div class="ver">0.4.2</div>
      <div class="mt-3"><Badge tone="success">HEALTHY</Badge></div>
    </StatCard>
  </div>
  <div class="card">
    <div class="card-header flex-between">
      <span>RECENT INDEXES</span>
      <Button variant="primary" size="sm">MANAGE ALL</Button>
    </div>
    <table class="table">
      <thead>
        <tr><th>Name</th><th>Documents</th><th>Root path</th><th>Status</th></tr>
      </thead>
      <tbody>
        {#each indexes as ix}
          <tr>
            <td class="name">{ix.name}</td>
            <td class="text-mono">{ix.docs}</td>
            <td class="path">{ix.path}</td>
            <td>
              {#if ix.status === 'working'}
                <Badge tone="warning" spinner>{ix.progress}</Badge>
              {:else}
                <Badge tone={statusBadge[ix.status][0]}>{statusBadge[ix.status][1]}</Badge>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  {#if toastVisible}
    <div class="toast-stack">
      <Toast tone="success" title="REINDEX COMPLETE" ondismiss={() => (toastVisible = false)}>
        memory-palace — 31,870 chunks in 2m 14s.
      </Toast>
    </div>
  {/if}
</AppShell>

<style>
  h1 { font-size: 28px; margin: 0 0 24px; }
  .four { grid-template-columns: repeat(4, 1fr); }
  .ver { font: 600 20px var(--trusty-mono); margin-top: 6px; }
  .card-header :global(.btn) { text-transform: uppercase; }
  .name { font-weight: 600; }
  .path { font: 400 11px var(--trusty-mono); color: var(--trusty-text-muted); }
  .toast-stack { position: absolute; }
</style>
