<script lang="ts">
  // Application identity, navigation, connection status, and theme.
  // Model selection lives beside the message it applies to, in InputArea.
  import { createEventDispatcher } from 'svelte';
  import Logo from '../lib/icons/Logo.svelte';
  import ThemeToggle from './ThemeToggle.svelte';
  import { isDesktop } from '../lib/transport';
  import {
    fetchBuildInfo,
    formatProvenance,
    provenanceTitle,
    type BuildInfoState,
  } from '../lib/buildInfo';

  // #3819: Chat/Projects/Personality → Chat/Events (Bob's nav reshape).
  // `AgentSwitcher` moved out of this header into `ChatHeader` (the chat
  // pane's own header — title + selector + gear all live together there
  // now); the model picker lives in the composer.
  // #4098: 'costs' added for the Costs tab (COST-09).
  // #4404: 'assistants' added — the landing picker, and the app's default view.
  export let activeView: 'assistants' | 'chat' | 'events' | 'costs' = 'assistants';
  export let apiReady = false;

  const desktop = isDesktop();
  const dispatch = createEventDispatcher<{ 'switch-view': { view: typeof activeView } }>();

  const tabs: { id: typeof activeView; label: string }[] = [
    // #4404: first, and the default — the picker is the landing view, so the
    // tab order matches the order a user meets the surfaces in.
    { id: 'assistants', label: 'Assistants' },
    { id: 'chat', label: 'Chat' },
    { id: 'events', label: 'Channels' },
    // #4098: spend by agent/model/day, from GET /api/costs.
    { id: 'costs', label: 'Cost' },
  ];

  function switchView(view: typeof activeView) {
    dispatch('switch-view', { view });
  }

  /**
   * Build provenance of the DAEMON this UI is talking to (owner ask: "we need
   * to show version in the header").
   *
   * Why fire on `apiReady` rather than `onMount`: on a cold start the sidecar
   * isn't listening when this component first renders (the same race
   * `catalogRefetch.ts` documents for the pickers), so a mount-time probe
   * would reliably fail and latch `unavailable`. `apiReady` flips true only
   * after `App.svelte`'s bootstrap loop has already seen a healthy
   * `check_health` — so this is a single follow-up read against a server known
   * to be up, NOT a second poller. `requestedBuildInfo` keeps it to one
   * request per app lifetime no matter how often this reactive block
   * re-evaluates.
   *
   * States (deliberate, see `buildInfo.ts`): `v…` before the probe answers —
   * never a compiled-in or guessed number, since a wrong version is worse than
   * no version for the stale-build diagnosis this exists to serve; `v—` if it
   * answers unusably. The slot is always rendered and width-reserved, so
   * neither transition shifts the lockup. If the API never becomes ready the
   * line stays `v…`, which is honest and already explained by the CONNECTING
   * badge on the right of this same row.
   *
   * #4260 (build provenance in `tagent --version` and `/api/health`) is filed
   * but NOT implemented: once it emits a commit, `parseHealthBody` picks it up
   * and this same single line renders `v0.38.6 · abc1234`. Nothing here needs
   * to change — add the SHA in `lib/buildInfo.ts`, not in this markup.
   */
  let buildInfo: BuildInfoState = { status: 'loading' };
  let requestedBuildInfo = false;

  $: if (apiReady && !requestedBuildInfo) {
    requestedBuildInfo = true;
    void fetchBuildInfo().then((info) => {
      buildInfo = info;
    });
  }
</script>

<header
  class="sticky top-0 z-20 flex h-20 w-full shrink-0 items-center justify-between border-b border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-4"
>
  <div class="flex items-center gap-3 min-w-0">
    <Logo height={50} />
    <!-- Provenance of the running daemon — reference information, so it is
         deliberately subordinate to the lockup: the badge row's 10px mono
         scale but with no border/background chip, and the muted text token
         (one variable per theme, so it reads correctly in light and dark).
         `min-w-[7ch]` reserves the width of a settled `v0.0.00` so the
         `v…` → `v0.38.6` transition can't nudge the lockup. -->
    <span
      class="shrink-0 min-w-[7ch] font-mono text-[10px] font-semibold tracking-wide text-foundry-light-muted dark:text-foundry-muted"
      title={provenanceTitle(buildInfo)}
      data-testid="build-provenance"
    >
      {formatProvenance(buildInfo)}
    </span>
  </div>

  <div class="flex items-center gap-2.5">
    <div
      class="flex h-8 items-center gap-1 rounded-md border border-foundry-light-border dark:border-foundry-border px-1"
      role="tablist"
      aria-label="View"
    >
      {#each tabs as tab (tab.id)}
        <button
          type="button"
          role="tab"
          aria-selected={activeView === tab.id}
          class="flex h-6 items-center rounded px-3 font-mono text-xs font-semibold uppercase tracking-wide transition-colors {activeView ===
          tab.id
            ? 'bg-foundry-light-primary/20 dark:bg-foundry-primary/20 text-foundry-light-primary dark:text-foundry-primary'
            : 'text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
          on:click={() => switchView(tab.id)}
        >
          {tab.label}
        </button>
      {/each}
    </div>


    <span
      class="flex h-8 items-center rounded-md border border-foundry-light-border dark:border-foundry-border px-2 font-mono text-[10px] font-semibold uppercase tracking-wide {desktop
        ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
        : 'bg-foundry-amber/15 text-foundry-amber'}"
      title={desktop ? 'Running inside Tauri (IPC)' : 'Running in browser (HTTP /api)'}
    >
      {desktop ? 'Desktop' : 'Web'}
    </span>

    <span
      class="inline-flex h-8 items-center gap-1.5 rounded-md border border-foundry-light-border dark:border-foundry-border px-2 font-mono text-[10px] font-semibold uppercase tracking-wide {apiReady
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
