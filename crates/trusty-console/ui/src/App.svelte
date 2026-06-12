<script>
  import { onMount } from 'svelte';
  import ServiceCard from './ServiceCard.svelte';
  import MemoryTab from './MemoryTab.svelte';
  import SearchTab from './SearchTab.svelte';
  import AnalyzeTab from './AnalyzeTab.svelte';
  import StubTab from './StubTab.svelte';

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
</script>

<main>
  <header>
    <h1>Trusty Console</h1>
    <p class="subtitle">Unified service dashboard</p>
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
            <ServiceCard {service} />
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
      <StubTab name="Review" endpoint={null} />
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
    max-width: 1100px;
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

  /* Tab bar */
  div.tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid #2d3348;
    margin-bottom: 1.5rem;
  }
  .tab-btn {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 0.6rem 1.2rem;
    color: #94a3b8;
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
    margin-bottom: -1px;
  }
  .tab-btn:hover {
    color: #e2e8f0;
  }
  .tab-btn.active {
    color: #7c3aed;
    border-bottom-color: #7c3aed;
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
    background: #1e2130;
    color: #94a3b8;
  }
  .error {
    color: #f87171;
  }
</style>
