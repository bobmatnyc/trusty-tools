<script>
  import { onMount } from 'svelte';
  import OverviewTab from './OverviewTab.svelte';
  import AnalyzeTab from './AnalyzeTab.svelte';
  import StubTab from './StubTab.svelte';

  let services = $state([]);
  let loading = $state(true);
  let error = $state(null);

  // activeTab: 'overview' | service id string
  let activeTab = $state('overview');

  // Map service id -> tab label for the four known services
  const SERVICE_TABS = [
    { id: 'trusty-search',  label: 'Search' },
    { id: 'trusty-memory',  label: 'Memory' },
    { id: 'trusty-analyze', label: 'Analyze' },
    { id: 'trusty-review',  label: 'Review' },
  ];

  onMount(async () => {
    try {
      const resp = await fetch('/api/console/services');
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      services = await resp.json();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  });

  function serviceById(id) {
    return services.find((s) => s.id === id) ?? null;
  }

  function switchTab(id) {
    activeTab = id;
  }
</script>

<main>
  <header>
    <h1>Trusty Console</h1>
    <p class="subtitle">Service status overview</p>
  </header>

  {#if !loading && !error}
    <div class="tabs" role="tablist">
      <button
        role="tab"
        aria-selected={activeTab === 'overview'}
        class="tab-btn"
        class:active={activeTab === 'overview'}
        onclick={() => switchTab('overview')}
      >
        Overview
      </button>
      {#each SERVICE_TABS as tab (tab.id)}
        <button
          role="tab"
          aria-selected={activeTab === tab.id}
          class="tab-btn"
          class:active={activeTab === tab.id}
          onclick={() => switchTab(tab.id)}
        >
          {tab.label}
        </button>
      {/each}
    </div>
  {/if}

  <div class="tab-content">
    {#if loading}
      <div class="loading">Detecting services…</div>
    {:else if error}
      <div class="error">Failed to load services: {error}</div>
    {:else if activeTab === 'overview'}
      <OverviewTab {services} onSelectService={switchTab} />
    {:else if activeTab === 'trusty-analyze'}
      <AnalyzeTab service={serviceById('trusty-analyze')} />
    {:else}
      {@const svc = serviceById(activeTab)}
      {@const tabMeta = SERVICE_TABS.find((t) => t.id === activeTab)}
      <StubTab service={svc} label={tabMeta?.label ?? activeTab} />
    {/if}
  </div>
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
  }
  :global(body) {
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: #0f1117;
    color: #e2e8f0;
    min-height: 100vh;
  }
  main {
    max-width: 1040px;
    margin: 0 auto;
    padding: 2rem 1rem;
  }
  header {
    margin-bottom: 1.5rem;
  }
  h1 {
    font-size: 2rem;
    font-weight: 700;
    margin: 0 0 0.25rem;
    background: linear-gradient(135deg, #7c3aed, #2563eb);
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
  }
  .subtitle {
    color: #94a3b8;
    margin: 0;
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid #2d3348;
    margin-bottom: 1.5rem;
    padding-bottom: 0;
  }
  .tab-btn {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: #94a3b8;
    cursor: pointer;
    font-size: 0.9rem;
    font-weight: 500;
    padding: 0.6rem 1rem;
    margin-bottom: -1px;
    transition: color 0.15s, border-color 0.15s;
  }
  .tab-btn:hover {
    color: #e2e8f0;
  }
  .tab-btn.active {
    color: #e2e8f0;
    border-bottom-color: #7c3aed;
  }
  .tab-content {
    min-height: 12rem;
  }
  .loading,
  .error {
    padding: 1.5rem;
    border-radius: 0.5rem;
    background: #1e2130;
    color: #94a3b8;
  }
  .error {
    color: #f87171;
  }
</style>
