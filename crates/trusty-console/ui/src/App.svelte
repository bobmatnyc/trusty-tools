<script>
  import { onMount } from 'svelte';
  import ServiceCard from './ServiceCard.svelte';
  import MemoryTab from './MemoryTab.svelte';
  import SearchTab from './SearchTab.svelte';
  import AnalyzeTab from './AnalyzeTab.svelte';
  import ReviewTab from './ReviewTab.svelte';
  import ThemeSelector from './ThemeSelector.svelte';

  // ── state ────────────────────────────────────────────────────────────────

  let services = $state([]);
  let loading = $state(true);
  let error = $state(null);
  let activeTab = $state('overview');

  const TABS = [
    { id: 'overview', label: 'Overview' },
    { id: 'search',   label: 'Search' },
    { id: 'memory',   label: 'Memory' },
    { id: 'analyze',  label: 'Analyze' },
    { id: 'review',   label: 'Review' },
  ];

  // Single source of truth: maps service.id → console tab key.
  // Services absent from this map will not show the "View details →" button.
  // ServiceCard derives its `hasTab` check from the key set of this map —
  // there is no separate TABBED_SERVICES literal anywhere in the codebase.
  const SERVICE_TAB_MAP = {
    'trusty-search':  'search',
    'trusty-memory':  'memory',
    'trusty-analyze': 'analyze',
    'trusty-review':  'review',
  };

  // Derived set used by ServiceCard to decide whether to render the button.
  // Kept in sync automatically — no manual mirroring required.
  const tabbedServices = new Set(Object.keys(SERVICE_TAB_MAP));

  // ── data fetch ───────────────────────────────────────────────────────────

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

  // ── navigation callback for ServiceCard overview buttons ─────────────────

  /** Switch to the tab for the given service id (if one exists). */
  function handleViewDetails(serviceId) {
    const tabKey = SERVICE_TAB_MAP[serviceId];
    if (tabKey) activeTab = tabKey;
  }
</script>

<main>
  <header>
    <div class="header-left">
      <h1>Trusty Console</h1>
      <p class="subtitle">Unified service dashboard</p>
    </div>
    <ThemeSelector />
  </header>

  <!-- Tab bar -->
  <div class="tabs" role="tablist">
    {#each TABS as tab (tab.id)}
      <button
        role="tab"
        class="tab-btn"
        class:active={activeTab === tab.id}
        aria-selected={activeTab === tab.id}
        onclick={() => activeTab = tab.id}
      >
        {tab.label}
      </button>
    {/each}
  </div>

  <!-- Tab panels -->
  <div class="panel">
    {#if activeTab === 'overview'}
      {#if loading}
        <div class="loading">Detecting services…</div>
      {:else if error}
        <div class="error">Failed to load services: {error}</div>
      {:else}
        <div class="cards">
          {#each services as service (service.id)}
            <ServiceCard
              {service}
              {tabbedServices}
              onViewDetails={tabbedServices.has(service.id) ? handleViewDetails : undefined}
            />
          {/each}
        </div>
      {/if}
    {:else if activeTab === 'search'}
      <SearchTab />
    {:else if activeTab === 'memory'}
      <MemoryTab />
    {:else if activeTab === 'analyze'}
      <AnalyzeTab />
    {:else if activeTab === 'review'}
      <ReviewTab />
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
    background: var(--color-bg);
    color: var(--color-text-primary);
    min-height: 100vh;
  }
  main {
    max-width: 1100px;
    margin: 0 auto;
    padding: 2rem 1rem;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
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
    color: var(--color-text-secondary);
    margin: 0;
  }
  .header-left {
    min-width: 0;
  }

  /* Tab bar */
  div.tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid var(--color-border);
    margin-bottom: 1.5rem;
  }
  .tab-btn {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 0.6rem 1.2rem;
    color: var(--color-text-secondary);
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
    margin-bottom: -1px;
  }
  .tab-btn:hover {
    color: var(--color-text-primary);
  }
  .tab-btn.active {
    color: var(--color-accent);
    border-bottom-color: var(--color-accent);
  }

  /* Panel */
  .panel {
    min-height: 200px;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 1rem;
  }
  .loading,
  .error {
    padding: 1.5rem;
    border-radius: 0.5rem;
    background: var(--color-surface);
    color: var(--color-text-secondary);
  }
  .error {
    color: var(--color-status-error);
  }
</style>
