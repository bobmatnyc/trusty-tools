<script lang="ts">
  /**
   * Why (#3819, epic #3052): Bob's directive moves the agent selector OUT of
   * the top toolbar (`AgentSwitcher`, formerly in `Header.svelte`) and INTO
   * the chat pane — the selected agent's display name IS the pane title,
   * consistent with the gear-icon directive (title + selector + gear all
   * live in one pane header). "Concierge" (the `ctrl` agent, `role =
   * "controller"`) is a fixed, pinned first entry, not part of the roster
   * fetch — selecting it sets `activeAgentId = null`, the pre-existing
   * "tools-armed base PM/ctrl session" dispatch path (`stores/app.ts`'s
   * `activeAgentId` doc comment), so this is a UI relabeling of an existing
   * state, not a new dispatch axis.
   * What: Renders the active selection's display name as an `<h1>`-style
   * pane title, a compact dropdown to switch it (Concierge pinned first,
   * then `$agentRoster` — "don't hardcode names, render whatever the roster
   * returns" per Bob), a "+ Add agent" row opening `AddAgentForm`, and a
   * gear button toggling `AgentConfigPanel` open/closed for whichever agent
   * is currently selected.
   * Test: Manual — open the app, confirm "Concierge" is the default title;
   * switch to Izzie via the dropdown, confirm the title updates; click the
   * gear, confirm `AgentConfigPanel` opens scoped to the selected agent.
   */
  import { onMount } from 'svelte';
  import { ChevronDown, Settings2, Plus, Bot } from 'lucide-svelte';
  import {
    activeAgentId,
    agentRoster,
    fetchAgentCatalog,
    refreshOverlayAgents,
  } from '../stores/app';
  import { rosterDisplayName } from '../lib/roster';
  import AgentConfigPanel from './AgentConfigPanel.svelte';
  import AddAgentForm from './AddAgentForm.svelte';

  const CONCIERGE_LABEL = 'Concierge';

  let open = false;
  let configOpen = false;
  let addingAgent = false;

  $: isConcierge = $activeAgentId === null;
  $: title = isConcierge ? CONCIERGE_LABEL : rosterDisplayName($agentRoster, $activeAgentId);
  $: configAgentName = isConcierge ? 'ctrl' : ($activeAgentId ?? 'assistant');

  function toggleDropdown() {
    open = !open;
    if (open) addingAgent = false;
  }

  function selectConcierge() {
    activeAgentId.set(null);
    open = false;
  }

  function selectRosterEntry(id: string) {
    activeAgentId.set(id);
    open = false;
  }

  function toggleConfig() {
    configOpen = !configOpen;
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

<div class="flex items-center justify-between border-b border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-4 py-2.5">
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
        <li>
          <button
            type="button"
            role="option"
            aria-selected={isConcierge}
            class="flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left text-xs transition-colors {isConcierge
              ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
              : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
            on:click={selectConcierge}
          >
            <span class="font-medium">{CONCIERGE_LABEL}</span>
            <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
              fixed coordination layer
            </span>
          </button>
        </li>
        {#each $agentRoster as entry (entry.id)}
          <li>
            <button
              type="button"
              role="option"
              aria-selected={!isConcierge && entry.id === $activeAgentId}
              class="flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left text-xs transition-colors {!isConcierge &&
              entry.id === $activeAgentId
                ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
                : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
              on:click={() => selectRosterEntry(entry.id)}
            >
              <span class="font-medium">{entry.label}</span>
              <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
                {entry.source === 'base' ? 'base' : entry.source === 'overlay' ? 'your overlay' : 'catalog'}
              </span>
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

  <button
    type="button"
    class="rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
    aria-label="Configure agent"
    aria-pressed={configOpen}
    on:click={toggleConfig}
  >
    <Settings2 class="h-4 w-4" />
  </button>
</div>

{#if configOpen}
  <div class="h-80 shrink-0 border-b border-foundry-light-border dark:border-foundry-border">
    <AgentConfigPanel agentName={configAgentName} isConcierge={isConcierge} />
  </div>
{/if}
