<script>
  /*
   * Why: Shell layout that mirrors trusty-memory — fixed dark sidebar on
   * the left, sticky topbar with breadcrumbs + version badge, and a
   * hash-routed content pane that renders one of three views.
   * What: Bootstraps the centralized state (health + indexes), then
   * dispatches the route to Dashboard / Search / Indexes.
   * Test: Open /ui in a browser, verify the three nav items render and the
   * version badge turns green once /health responds.
   */
  import Sidebar from './lib/components/Sidebar.svelte';
  import Topbar from './lib/components/Topbar.svelte';
  import Dashboard from './lib/views/Dashboard.svelte';
  import Search from './lib/views/Search.svelte';
  import Indexes from './lib/views/Indexes.svelte';
  import IndexConfig from './lib/views/IndexConfig.svelte';
  import Cleanup from './lib/views/Cleanup.svelte';
  import Config from './lib/views/Config.svelte';
  import Health from './lib/views/Health.svelte';
  import Logs from './lib/views/Logs.svelte';
  import { getRoute } from './lib/router.svelte.js';
  import { resolveView } from './lib/routes.js';
  import { refreshHealth, refreshIndexes } from './lib/state.svelte.js';
  import { onMount } from 'svelte';

  let bootError = $state(null);

  onMount(() => {
    refreshHealth().catch((e) => {
      bootError = e.message || String(e);
    });
    refreshIndexes().catch(() => {});
    // Poll /health every 10s so the version badge stays live.
    const t = setInterval(() => {
      refreshHealth().catch(() => {});
    }, 10_000);
    return () => clearInterval(t);
  });

  let route = $derived(getRoute());

  // #6941: the dispatch moved to `lib/routes.js` so its ORDER is testable — the
  // roster arm matches `#/indexes/*` on the first segment alone, and the cleanup
  // route only reaches its view by sitting ahead of it.
  let view = $derived(resolveView(route.segments));
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
              Make sure trusty-search is running with
              <code>trusty-search serve</code>.
            </p>
          </div>
        </div>
      {:else if view.kind === 'dashboard'}
        <Dashboard />
      {:else if view.kind === 'search'}
        <Search />
      {:else if view.kind === 'indexes'}
        <Indexes />
      {:else if view.kind === 'cleanup'}
        <Cleanup />
      {:else if view.kind === 'index-config'}
        <IndexConfig id={view.id} />
      {:else if view.kind === 'config'}
        <Config />
      {:else if view.kind === 'health'}
        <Health />
      {:else if view.kind === 'logs'}
        <Logs />
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
