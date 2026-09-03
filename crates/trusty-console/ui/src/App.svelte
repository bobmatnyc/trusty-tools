<script>
  import { onMount, onDestroy } from 'svelte';
  // #6642: the Overview tab is the whole-machine dashboard followed by ONE
  // services section. The "Installed Services" ServiceCard grid and the
  // dashboard's own rollup table were the two duplicates the owner's ruling
  // removed; `ServicesList` replaces both.
  import MachineStatusPanel from './MachineStatusPanel.svelte';
  import ServicesList from './ServicesList.svelte';
  import MemoryTab from './MemoryTab.svelte';
  import SearchTab from './SearchTab.svelte';
  import AnalyzeTab from './AnalyzeTab.svelte';
  import ReviewTab from './ReviewTab.svelte';
  import SessionsTab from './SessionsTab.svelte';
  import ConfigTab from './ConfigTab.svelte';
  import ThemeSelector from './ThemeSelector.svelte';
  import BrandLockup from './BrandLockup.svelte';
  // #6519: opt-in idle entry to the screensaver route.
  import {
    IDLE_ENTRY_URL,
    IDLE_EVENTS,
    idleExpiredAt,
    readIdleMinutes,
  } from './screensaver.js';
  // #6642: one EventSource for the whole page — see machineStream.js.
  import { createMachineStream, initialState } from './machineStream.js';

  // ── state ────────────────────────────────────────────────────────────────

  let services = $state([]);
  let loading = $state(true);
  let error = $state(null);
  let activeTab = $state('overview');
  // #6642: the 1 s machine-status window every graph on this page draws from,
  // and the single stream that fills it. Not `$state` — nothing renders it.
  let history = $state(initialState());
  let stream;

  const TABS = [
    { id: 'overview', label: 'Overview' },
    { id: 'search',   label: 'Search' },
    { id: 'memory',   label: 'Memory' },
    { id: 'analyze',  label: 'Analyze' },
    { id: 'review',   label: 'Review' },
    { id: 'sessions', label: 'MPM Sessions' }, // #6370: UI label only — the id stays 'sessions'
    { id: 'config',   label: 'Config' },
  ];

  // Single source of truth: maps service.id → console tab key. A service absent
  // from this map has no dashboard, so `ServicesList` renders its row inert —
  // there is no separate TABBED_SERVICES literal anywhere in the codebase.
  const SERVICE_TAB_MAP = {
    'trusty-search':  'search',
    'trusty-memory':  'memory',
    'trusty-analyze': 'analyze',
    'trusty-review':  'review',
    'trusty-mpm':     'sessions',
  };

  // Derived set `ServicesList` asks whether a row is clickable. Kept in sync
  // automatically — no manual mirroring required.
  const tabbedServices = new Set(Object.keys(SERVICE_TAB_MAP));

  // ── data fetch ───────────────────────────────────────────────────────────

  // #6642: the roster is fetched once so the list is not blank before the
  // stream connects; `cpu_pct` already rides on it, so the %CPU column is
  // populated on first paint and the graphs fill in as samples arrive.
  onMount(async () => {
    armIdleWatch();
    stream = createMachineStream({ onState: (next) => (history = next) });
    stream.start();
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
    stream?.stop();
    clearInterval(idleTimer);
    for (const event of IDLE_EVENTS) {
      window.removeEventListener(event, noteInput);
    }
  });

  // ── navigation callback for the Services list ────────────────────────────

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
      <MachineStatusPanel samples={history.samples} />
      <!-- #6642: the one services section on the page. -->
      <ServicesList
        {services}
        serviceSamples={history.serviceSamples}
        dashboards={tabbedServices}
        onOpen={handleViewDetails}
        {loading}
        {error}
      />
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
</style>
