<script>
  import { onMount, onDestroy } from 'svelte';
  import ServiceCard from './ServiceCard.svelte';
  // #6518: the Overview tab leads with the whole-machine dashboard; the card
  // grid below it stays because it is the only place a service that never
  // reported — absent binary, never installed — is visible at all.
  import MachineStatusPanel from './MachineStatusPanel.svelte';
  import MemoryTab from './MemoryTab.svelte';
  import SearchTab from './SearchTab.svelte';
  import AnalyzeTab from './AnalyzeTab.svelte';
  import ReviewTab from './ReviewTab.svelte';
  import SessionsTab from './SessionsTab.svelte';
  import ConfigTab from './ConfigTab.svelte';
  import ThemeSelector from './ThemeSelector.svelte';
  import BrandLockup from './BrandLockup.svelte';
  import BrandMark from './BrandMark.svelte';
  // #6519: opt-in idle entry to the screensaver route.
  import {
    IDLE_ENTRY_URL,
    IDLE_EVENTS,
    idleExpiredAt,
    readIdleMinutes,
  } from './screensaver.js';

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
    { id: 'sessions', label: 'MPM Sessions' }, // #6370: UI label only — the id stays 'sessions'
    { id: 'config',   label: 'Config' },
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
    'trusty-mpm':     'sessions',
  };

  // Derived set used by ServiceCard to decide whether to render the button.
  // Kept in sync automatically — no manual mirroring required.
  const tabbedServices = new Set(Object.keys(SERVICE_TAB_MAP));

  // ── data fetch ───────────────────────────────────────────────────────────

  onMount(async () => {
    armIdleWatch();
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

  // ── idle entry to the screensaver (#6519) ────────────────────────────────

  /** How often the idle check runs. Minutes-scale threshold, so this is cheap. */
  const IDLE_TICK_MS = 10_000;

  let lastInputMs = Date.now();
  let idleTimer;
  const noteInput = () => (lastInputMs = Date.now());

  /**
   * Watch for idleness, but only when the operator asked for it.
   *
   * Default OFF: `readIdleMinutes()` returns 0 unless
   * `trusty-console-screensaver-idle-minutes` is set in localStorage, and this
   * function then attaches nothing. A console that navigates itself away
   * mid-read is worse than no screensaver, so the feature stays opt-in until it
   * earns a settings UI.
   */
  function armIdleWatch() {
    const minutes = readIdleMinutes();
    if (minutes <= 0) return;
    for (const event of IDLE_EVENTS) {
      window.addEventListener(event, noteInput, { passive: true });
    }
    idleTimer = setInterval(() => {
      if (idleExpiredAt(lastInputMs, Date.now(), minutes)) {
        window.location.assign(IDLE_ENTRY_URL);
      }
    }, IDLE_TICK_MS);
  }

  onDestroy(() => {
    clearInterval(idleTimer);
    for (const event of IDLE_EVENTS) {
      window.removeEventListener(event, noteInput);
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
      <h1><BrandLockup /></h1>
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
      <!-- The dashboard reads its own endpoint, so it renders whether or not
           the service probe below has answered. -->
      <MachineStatusPanel />
      <!-- Not "Services": the dashboard above already has a SERVICES card, and
           two identical headings on one screen read as a duplicate. This grid
           lists what is INSTALLED, reporting or not. -->
      <h2 class="section-title">Installed Services</h2>
      {#if loading}
        <div class="loading">
          <BrandMark size={28} />
          <span>Detecting services…</span>
        </div>
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
    {:else if activeTab === 'sessions'}
      <SessionsTab />
    {:else if activeTab === 'config'}
      <ConfigTab />
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
    background: var(--trusty-content-bg);
    color: var(--trusty-text-primary);
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
  /* The brand lockup owns its own type scale and color (BrandLockup.svelte);
     the heading exists for document structure only. The previous gradient
     wordmark is gone — the Foundry identity is flat, no gradients
     (docs/design/UI/icons/README.md). */
  h1 {
    margin: 0;
    font-size: inherit;
    font-weight: inherit;
  }
  .header-left {
    min-width: 0;
  }

  /* Tab bar */
  div.tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid var(--trusty-border);
    margin-bottom: 1.5rem;
  }
  .tab-btn {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 0.6rem 1.2rem;
    color: var(--trusty-text-secondary);
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
    margin-bottom: -1px;
  }
  .tab-btn:hover {
    color: var(--trusty-text-primary);
  }
  .tab-btn.active {
    color: var(--trusty-accent);
    border-bottom-color: var(--trusty-accent);
  }

  /* Panel */
  .panel {
    min-height: 200px;
  }
  /* #6518: names the surviving ServiceCard grid now that the machine-status
     dashboard sits above it. Same Foundry display treatment as the dashboard's
     own heading. */
  .section-title {
    margin: 0 0 1rem;
    font-family: var(--trusty-display);
    font-size: 1.25rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--trusty-text-primary);
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
    background: var(--trusty-card-bg);
    color: var(--trusty-text-secondary);
  }
  .loading {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .error { color: var(--trusty-danger); }
</style>
