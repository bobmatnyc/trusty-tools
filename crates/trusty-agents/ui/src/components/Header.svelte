<script lang="ts">
  /**
   * Why (#3220 — Trusty Agents header consolidation, epic #3052): the
   * app previously spread its "single source of orientation" state across
   * three places — a bare logo+theme-toggle Header, a separate `<nav>` tab
   * bar inside App.svelte's `<main>`, and a floating Desktop/Web pill
   * absolutely-positioned over the whole page. The Foundry mockup
   * (`docs/design/gui/Foundry Ecosystem.dc.html:140-161`) integrates all of
   * that into one header: wordmark + tagline on the left, view tabs +
   * status badges + the agent roster switcher on the right. Consolidating
   * here means there is exactly one place that answers "what app is this,
   * what am I looking at, is it connected, and who am I talking to."
   * Header height is `h-20` (80px, up from 64px then 52px per two rounds of
   * owner visual feedback on #3479) so the 50px-tall `<Logo>` lockup keeps
   * the Foundry clear-space rule (~one robot-eye width) above/below it
   * instead of touching the header's top/bottom border.
   * What: Renders the Trusty Agents brand lockup (`<Logo>` — mark +
   * "TRUSTY AGENTS" wordmark + "UNIT-04 · MPM ORCHESTRATION" descriptor,
   * theme-aware per `docs/design/UI/icons/README.md`) on the left. On the
   * right: the view-switch tabs (Chat/Projects/Personality — App.svelte owns
   * `activeView`; this component only dispatches `switch-view` so
   * App.svelte's unsaved-Personality-buffer guard in `switchView()` stays
   * the single gate on navigation), the `AgentSwitcher` roster dropdown
   * (#3223), the `ModelSwitcher` model/provider picker (#3245), a
   * DESKTOP/WEB transport badge (replaces the old floating pill), an API
   * READY/CONNECTING status badge driven by `apiReady`, and `ThemeToggle`.
   * Every one of those right-hand controls (tabs container, `AgentSwitcher`,
   * `ModelSwitcher`, the two status badges, `ThemeToggle`) shares one border
   * standard — `border border-foundry-light-border dark:border-foundry-border`
   * + `rounded-md` — per owner visual feedback that the borders read as
   * inconsistent widths/radii (some controls had no border at all, one used
   * a translucent accent border, `ThemeToggle` used a larger radius with no
   * border). Semantic state (active tab, ready/connecting, desktop/web)
   * is now carried entirely by background/text/dot color, never by
   * a differently-weighted border.
   * Test: Mount `<Header activeView="chat" apiReady={true} />`, verify the
   * CHAT tab is highlighted, the API READY badge shows green, and clicking
   * PROJECTS dispatches `switch-view` with `{ view: 'projects' }`. Manual:
   * toggle `apiReady` false→true, confirm the badge flips
   * CONNECTING→API READY.
   */
  import { createEventDispatcher } from 'svelte';
  import Logo from '../lib/icons/Logo.svelte';
  import ThemeToggle from './ThemeToggle.svelte';
  import AgentSwitcher from './AgentSwitcher.svelte';
  import ModelSwitcher from './ModelSwitcher.svelte';
  import { isDesktop } from '../lib/transport';

  export let activeView: 'chat' | 'projects' | 'personality' = 'chat';
  export let apiReady = false;

  const desktop = isDesktop();
  const dispatch = createEventDispatcher<{ 'switch-view': { view: typeof activeView } }>();

  const tabs: { id: typeof activeView; label: string }[] = [
    { id: 'chat', label: 'Chat' },
    { id: 'projects', label: 'Projects' },
    { id: 'personality', label: 'Personality' },
  ];

  function switchView(view: typeof activeView) {
    dispatch('switch-view', { view });
  }
</script>

<header
  class="sticky top-0 z-20 flex h-20 w-full shrink-0 items-center justify-between border-b border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-4"
>
  <div class="flex items-center gap-3 min-w-0">
    <Logo height={50} />
  </div>

  <div class="flex items-center gap-2.5">
    <div
      class="flex items-center gap-1 rounded-md border border-foundry-light-border dark:border-foundry-border px-1 py-1"
      role="tablist"
      aria-label="View"
    >
      {#each tabs as tab (tab.id)}
        <button
          type="button"
          role="tab"
          aria-selected={activeView === tab.id}
          class="rounded px-3 py-1 font-mono text-xs font-semibold uppercase tracking-wide transition-colors {activeView ===
          tab.id
            ? 'bg-foundry-light-primary/20 dark:bg-foundry-primary/20 text-foundry-light-primary dark:text-foundry-primary'
            : 'text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
          on:click={() => switchView(tab.id)}
        >
          {tab.label}
        </button>
      {/each}
    </div>

    <AgentSwitcher />
    <ModelSwitcher />

    <span
      class="rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide {desktop
        ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
        : 'bg-foundry-amber/15 text-foundry-amber'}"
      title={desktop ? 'Running inside Tauri (IPC)' : 'Running in browser (HTTP /api)'}
    >
      {desktop ? 'Desktop' : 'Web'}
    </span>

    <span
      class="inline-flex items-center gap-1.5 rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide {apiReady
        ? 'bg-green-500/15 text-green-600 dark:text-green-400'
        : 'bg-foundry-light-surface dark:bg-foundry-surface text-foundry-light-muted dark:text-foundry-text/50'}"
    >
      <span
        class="inline-block h-1.5 w-1.5 rounded-full {apiReady
          ? 'bg-green-500 dark:bg-green-400'
          : 'bg-foundry-light-muted dark:bg-foundry-text/40'}"
        aria-hidden="true"
      ></span>
      {apiReady ? 'API Ready' : 'Connecting'}
    </span>

    <ThemeToggle />
  </div>
</header>
