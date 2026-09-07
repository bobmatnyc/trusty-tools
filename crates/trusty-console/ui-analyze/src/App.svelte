<script>
  /*
   * Why: Shell layout that mirrors trusty-memory — fixed dark sidebar on
   * the left, sticky topbar, hash-routed content pane that renders one of
   * six views, plus a long-lived SSE connection that keeps every pane fresh.
   * What: Bootstraps the centralized state (health + indexes), opens the
   * /sse stream, and dispatches the route to Dashboard / Complexity /
   * Smells / Refactors / Clusters / Facts.
   * Test: Open /ui in a browser, verify nav items render and the health
   * pill turns green once /health responds.
   */
  import Sidebar from './lib/components/Sidebar.svelte';
  import Topbar from './lib/components/Topbar.svelte';
  import Dashboard from './lib/views/Dashboard.svelte';
  import Complexity from './lib/views/Complexity.svelte';
  import Smells from './lib/views/Smells.svelte';
  import Refactors from './lib/views/Refactors.svelte';
  import Clusters from './lib/views/Clusters.svelte';
  import Facts from './lib/views/Facts.svelte';
  import { getRoute } from './lib/router.svelte.js';
  import {
    refreshHealth,
    refreshIndexes,
    applyTheme,
    getTheme
  } from './lib/state.svelte.js';
  import { onMount } from 'svelte';

  let bootError = $state(null);

  // Re-apply theme whenever the user preference changes (also handles
  // first-paint sync for the data-theme attribute on <html>).
  $effect(() => {
    applyTheme(getTheme());
  });

  onMount(async () => {
    try {
      await Promise.all([refreshHealth(), refreshIndexes()]);
    } catch (e) {
      bootError = e.message || String(e);
    }
    // #6155: polling is the only liveness signal left. #6287 deleted this
    // daemon's `/sse` broadcast along with its HTTP listener and put nothing
    // streaming in its place, so the EventSource this component used to open
    // subscribed to a route no daemon answers.
    const t = setInterval(() => {
      refreshHealth().catch(() => {});
    }, 10_000);
    return () => clearInterval(t);
  });

  let route = $derived(getRoute());

  let view = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return 'dashboard';
    const head = segs[0];
    if (head === 'complexity') return 'complexity';
    if (head === 'smells') return 'smells';
    if (head === 'refactors') return 'refactors';
    if (head === 'clusters') return 'clusters';
    if (head === 'facts') return 'facts';
    return 'dashboard';
  });
</script>

<div class="layout">
  <Sidebar />
  <div class="main">
    <Topbar />
    <div class="content">
      {#if bootError}
        <div class="card" style="border-color: var(--trusty-danger)">
          <div class="card-header" style="color: var(--trusty-danger)">
            Connection error
          </div>
          <div class="card-body">
            <p>{bootError}</p>
            <p class="text-muted text-sm">
              Make sure trusty-analyzer is running with
              <code>trusty-analyzer serve</code> and that trusty-search is
              reachable on port 7878.
            </p>
          </div>
        </div>
      {:else if view === 'dashboard'}
        <Dashboard />
      {:else if view === 'complexity'}
        <Complexity />
      {:else if view === 'smells'}
        <Smells />
      {:else if view === 'refactors'}
        <Refactors />
      {:else if view === 'clusters'}
        <Clusters />
      {:else if view === 'facts'}
        <Facts />
      {/if}
    </div>
  </div>
</div>

<style>
  .layout {
    display: flex;
    min-height: 100vh;
  }
  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    margin-left: var(--trusty-sidebar-width);
    min-width: 0;
  }
  .content {
    padding: var(--trusty-space-5) var(--trusty-space-6);
    flex: 1;
    min-width: 0;
  }
</style>
