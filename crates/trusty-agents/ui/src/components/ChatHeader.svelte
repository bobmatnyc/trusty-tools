<script lang="ts">
  /** The chat title and switcher show user-facing assistants only.
   * Concierge remains an internal configuration helper, never a picker option.
   * Configuration stays mounted through ChatPane to preserve conversation state.
   */
  import { onMount } from 'svelte';
  import { ChevronDown, Settings2, Plus, Bot, Network } from 'lucide-svelte';
  import {
    activeAgentId,
    agentRoster,
    fetchAgentCatalog,
    refreshOverlayAgents,
  } from '../stores/app';
  import { CONCIERGE_AGENT_ID, rosterDisplayName } from '../lib/roster';
  import { configPaneOpen, openConfigPane, requestExitConfigPane } from '../stores/configPane';
  import AddAgentForm from './AddAgentForm.svelte';
  export let onOpenKnowledgeGraph: () => void = () => {};

  let open = false;
  let addingAgent = false;


  $: hasSelectedAssistant = $activeAgentId !== null;
  $: title = rosterDisplayName($agentRoster, $activeAgentId);
  $: assistantEntries = $agentRoster.filter((e) => e.id !== CONCIERGE_AGENT_ID);

  function toggleDropdown() {
    open = !open;
    if (open) addingAgent = false;
  }

  /**
   * Why (PR #3895 re-review): the gear is an EXIT affordance too — pressing it
   * while the takeover is up leaves configuration — so it must pass the same
   * unsaved-edits guard as Esc/Back/Close. A plain `toggleConfigPane()` here
   * flipped `configPaneOpen` straight to false, silently discarding a dirty
   * panel. `inert` on the covered chat makes that click unreachable in a
   * standards-compliant browser, but a data-loss guard cannot rest on a CSS/DOM
   * property whose behavior in the Tauri WKWebView is unverified — it belongs
   * in the state logic. (`toggleConfigPane` was deleted with this change so no
   * future caller can reintroduce the bypass.)
   * What: Opens directly; leaves via `requestExitConfigPane`, which raises the
   * panel's confirm when there is something unsaved to lose.
   * Test: `ChatPane.test.ts` — "the gear cannot silently discard a dirty panel".
   */
  function handleGear() {
    if ($configPaneOpen) {
      requestExitConfigPane();
    } else {
      openConfigPane();
    }
  }

  function selectRosterEntry(id: string) {
    activeAgentId.set(id);
    open = false;
  }

  function handleWindowClick(event: MouseEvent) {
    if (!open) return;
    const target = event.target as HTMLElement;
    if (!target.closest('[data-chat-header-switcher]')) {
      open = false;
    }
  }

  function onAgentCreated(event: CustomEvent<{ name: string }>) {
    addingAgent = false;
    open = false;
    activeAgentId.set(event.detail.name);
    fetchAgentCatalog().catch((e) => console.error('[ChatHeader] refresh failed:', e));
  }

  onMount(() => {
    fetchAgentCatalog().catch((e) => console.error('[ChatHeader] fetchAgentCatalog failed:', e));
    refreshOverlayAgents();
  });
</script>

<svelte:window on:click={handleWindowClick} />

<div class="flex shrink-0 min-w-0 items-center justify-between border-b border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-4 py-2.5">
  <div class="relative inline-block" data-chat-header-switcher>
    <button
      type="button"
      class="flex items-center gap-2 rounded-md px-2 py-1 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10"
      on:click={toggleDropdown}
      aria-haspopup="listbox"
      aria-expanded={open}
    >
      <Bot class="h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
      <h1 class="text-sm font-semibold text-foundry-light-text dark:text-foundry-text">{title}</h1>
      <ChevronDown class="h-3.5 w-3.5 opacity-60" />
    </button>

    {#if open}
      <ul
        role="listbox"
        class="absolute left-0 top-full z-30 mt-1 max-h-80 w-64 overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface py-1 shadow-lg"
      >
        <li class="px-3 pt-1.5 pb-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          Assistants
        </li>
        {#each assistantEntries as entry (entry.id)}
          <li>
            <button
              type="button"
              role="option"
              aria-selected={entry.id === $activeAgentId}
              class="flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left text-xs transition-colors {entry.id === $activeAgentId
                ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
                : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
              on:click={() => selectRosterEntry(entry.id)}
            >
              <span class="font-medium">{entry.label}</span>
              <!-- #3819 (Bob review): the tier label ("catalog") was noise
                   for local directory-package agents — a real, meaningful
                   subtitle (the agent's own description) or nothing, never
                   an internal storage-tier name. "your overlay" stays for
                   overlay entries since it IS meaningful there (distinguishes
                   a personal customization from a bundled persona). -->
              {#if entry.source === 'overlay'}
                <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
                  your overlay
                </span>
              {:else if entry.description}
                <span class="text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
                  {entry.description}
                </span>
              {/if}
            </button>
          </li>
        {/each}
        <li class="border-t border-foundry-light-border dark:border-foundry-border mt-1 pt-1">
          {#if addingAgent}
            <div class="px-2 pb-1">
              <AddAgentForm on:created={onAgentCreated} on:cancel={() => (addingAgent = false)} />
            </div>
          {:else}
            <button
              type="button"
              class="flex w-full items-center gap-1.5 px-3 py-1.5 text-left text-xs text-foundry-light-primary dark:text-foundry-primary hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10"
              on:click={() => (addingAgent = true)}
            >
              <Plus class="h-3.5 w-3.5" /> Add agent
            </button>
          {/if}
        </li>
      </ul>
    {/if}
  </div>

  <span class="ml-auto flex items-center gap-1">
    <!-- A knowledge graph requires an explicitly selected assistant. -->
    {#if hasSelectedAssistant}
      <button
        type="button"
        class="rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
        aria-label="Knowledge Graph"
        title="Knowledge Graph"
        on:click={onOpenKnowledgeGraph}
      >
        <Network class="h-4 w-4" />
      </button>
    {/if}

    <button
      type="button"
      class="rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
      aria-label="Configure agent"
      aria-pressed={$configPaneOpen}
      on:click={handleGear}
    >
      <Settings2 class="h-4 w-4" />
    </button>
  </span>
</div>
