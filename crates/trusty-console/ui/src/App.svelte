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
  // #6643: the roster read the screensaver shares.
  import { fetchServices } from './servicesList.js';
  // #6909: the labels the tab bar used to carry, now the breadcrumb's.
  import { viewLabel } from './consoleNav.js';

  // ── state ────────────────────────────────────────────────────────────────

  let services = $state([]);
  let loading = $state(true);
  let error = $state(null);
  // #6909: which view the panel renders. The tab bar that used to set this is
  // gone — the Services list sets it for a service, the header's Config action
  // sets it for Config, and the breadcrumb sets it back to 'overview'.
  let view = $state('overview');
  // #6642: the 1 s machine-status window every graph on this page draws from,
  // and the single stream that fills it. Not `$state` — nothing renders it.
  let history = $state(initialState());
  let stream;

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
    const roster = await fetchServices();
    services = roster.services;
    error = roster.error;
    loading = false;
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

  /** Open the view for the given service id (if one exists). */
  function handleViewDetails(serviceId) {
    const tabKey = SERVICE_TAB_MAP[serviceId];
    if (tabKey) view = tabKey;
  }
</script>

<main>
  <!-- #6909: the Foundry app shell puts a breadcrumb on the left of the topbar
       and its actions on the right (docs/design/UI/design-system-svelte
       lib/Topbar.svelte). This header is that topbar: brand and breadcrumb
       left, Config action and theme control right. No tab strip. -->
  <header>
    <div class="header-left">
      <h1><BrandLockup /></h1>
      {#if view !== 'overview'}
        <nav class="crumbs" aria-label="Breadcrumb">
          <span class="crumb-prefix" aria-hidden="true">//</span>
          <ol>
            <li>
              <button type="button" class="crumb-link" onclick={() => (view = 'overview')}>
                Overview
              </button>
            </li>
            <li aria-current="page">
              <span class="crumb-sep" aria-hidden="true">/</span>{viewLabel(view)}
            </li>
          </ol>
        </nav>
      {/if}
    </div>
    <div class="header-right">
      <!-- #6909: Config is the one former tab with no Services row, so it keeps
           a single header action rather than a re-created tab strip. -->
      <button
        type="button"
        class="header-action"
        class:active={view === 'config'}
        aria-current={view === 'config' ? 'page' : undefined}
        onclick={() => (view = 'config')}
      >
        Config
      </button>
      <ThemeSelector />
    </div>
  </header>

  <!-- View panel -->
  <div class="panel">
    {#if view === 'overview'}
      <!-- The dashboard reads its own endpoint, so it renders whether or not
           the service probe below has answered. -->
      <MachineStatusPanel samples={history.samples} />
      <!-- #6642: the one services section on the page, and since #6909 the
           only navigation to a service view. -->
      <ServicesList
        {services}
        serviceSamples={history.serviceSamples}
        dashboards={tabbedServices}
        onOpen={handleViewDetails}
        {loading}
        {error}
      />
    {:else if view === 'search'}
      <SearchTab />
    {:else if view === 'memory'}
      <MemoryTab />
    {:else if view === 'analyze'}
      <AnalyzeTab />
    {:else if view === 'review'}
      <ReviewTab />
    {:else if view === 'sessions'}
      <SessionsTab />
    {:else if view === 'config'}
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
  .header-right {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  /* Breadcrumb (#6909) — the Foundry topbar's `// crumb`, mono and stamped. */
  .crumbs {
    display: flex;
    align-items: baseline;
    gap: 0.35rem;
    margin-top: 0.4rem;
  }
  .crumbs ol {
    display: flex;
    align-items: baseline;
    gap: 0.35rem;
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .crumb-prefix,
  .crumbs li {
    font-family: var(--trusty-mono);
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--trusty-text-secondary);
  }
  .crumbs li[aria-current='page'] {
    color: var(--trusty-text-primary);
  }
  .crumb-sep {
    margin-right: 0.35rem;
    color: var(--trusty-text-secondary);
  }
  .crumb-link {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    cursor: pointer;
    border-bottom: 1px solid transparent;
    transition: color 0.15s, border-color 0.15s;
  }
  .crumb-link:hover,
  .crumb-link:focus-visible {
    color: var(--trusty-accent);
    border-bottom-color: var(--trusty-accent);
  }

  /* Config entry point (#6909) — a header action, sized to the theme control
     beside it so the two read as one row rather than a leftover tab. */
  .header-action {
    background: none;
    border: 1px solid var(--trusty-border);
    border-radius: 0.4rem;
    color: var(--trusty-text-secondary);
    cursor: pointer;
    font-size: 0.72rem;
    font-weight: 500;
    line-height: 1.4;
    padding: 0.25rem 0.6rem;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }
  .header-action:hover {
    color: var(--trusty-text-primary);
  }
  .header-action.active {
    border-color: var(--trusty-accent);
    color: var(--trusty-accent);
  }

  /* Panel */
  .panel {
    min-height: 200px;
  }
</style>
