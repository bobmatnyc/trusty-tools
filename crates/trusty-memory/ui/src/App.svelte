<script>
  /*
   * Why: Shell layout for the trusty-memory admin UI — fixed dark sidebar,
   * sticky topbar with breadcrumbs + service controls, and a hash-routed
   * content pane. Mirrors the trusty-search shell so operators jumping
   * between the two tools get a consistent experience.
   * What: Bootstraps shared health state, then dispatches the route to
   * Health / Palaces / Logs / Dream.
   * Test: open the SPA, verify the four nav items render and the version
   * badge turns green once /health responds.
   */
  import Sidebar from './lib/components/Sidebar.svelte';
  import Topbar from './lib/components/Topbar.svelte';
  import ActivityFeed from './lib/components/ActivityFeed.svelte';
  import Health from './lib/views/Health.svelte';
  import Palaces from './lib/views/Palaces.svelte';
  import Logs from './lib/views/Logs.svelte';
  import Dream from './lib/views/Dream.svelte';
  import KG from './lib/views/KG.svelte';
  import PalaceGraph from './lib/views/PalaceGraph.svelte';
  import { getRoute } from './lib/router.svelte.js';
  import { refreshHealth, refreshStatus } from './lib/state.svelte.js';
  import { onMount } from 'svelte';

  let bootError = $state(null);

  onMount(() => {
    refreshHealth().catch((e) => {
      bootError = e.message || String(e);
    });
    refreshStatus().catch(() => {});
    // Poll /health every 10s so the version badge stays live.
    const t = setInterval(() => {
      refreshHealth().catch(() => {});
    }, 10_000);
    return () => clearInterval(t);
  });

  let route = $derived(getRoute());

  let view = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return { kind: 'health' };
    // Issue #97: `/palace/<id>/graph` opens the per-palace graph view.
    // `/palaces` (plural) stays on the existing list view.
    if (segs[0] === 'palace' && segs.length >= 2 && segs[2] === 'graph') {
      return { kind: 'palace-graph' };
    }
    if (segs[0] === 'palaces' || segs[0] === 'palace') return { kind: 'palaces' };
    if (segs[0] === 'kg') return { kind: 'kg' };
    if (segs[0] === 'logs') return { kind: 'logs' };
    if (segs[0] === 'dream') return { kind: 'dream' };
    if (segs[0] === 'health') return { kind: 'health' };
    return { kind: 'health' };
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
              Make sure trusty-memory is running with
              <code>trusty-memory serve --http 127.0.0.1:7079</code>.
            </p>
          </div>
        </div>
      {:else if view.kind === 'health'}
        <Health />
      {:else if view.kind === 'palace-graph'}
        <PalaceGraph />
      {:else if view.kind === 'palaces'}
        <Palaces />
      {:else if view.kind === 'logs'}
        <Logs />
      {:else if view.kind === 'kg'}
        <KG />
      {:else if view.kind === 'dream'}
        <Dream />
      {/if}
    </div>
  </div>
  <ActivityFeed />
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
    /* Reserve space for the fixed-position ActivityFeed on the right. */
    margin-right: 320px;
    min-width: 0;
  }
  .content {
    padding: var(--trusty-space-5) var(--trusty-space-6);
    flex: 1;
    min-width: 0;
  }
</style>
