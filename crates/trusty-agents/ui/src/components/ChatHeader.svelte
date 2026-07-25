<script lang="ts">
  /**
   * Why (#3819, epic #3052): Bob's directive moves the agent selector OUT of
   * the top toolbar (`AgentSwitcher`, formerly in `Header.svelte`) and INTO
   * the chat pane — the selected agent's display name IS the pane title,
   * consistent with the gear-icon directive (title + selector + gear all
   * live in one pane header). "Concierge" (the `ctrl` agent) selects via
   * `activeAgentId = null`, the pre-existing "tools-armed base PM/ctrl
   * session" dispatch path (`stores/app.ts`'s `activeAgentId` doc comment) —
   * load-bearing: dispatching by NAME (`agent: "ctrl"`, the normal roster
   * path) routes through the tools-OFF persona-chat path instead
   * (`handlers.rs::resolve_agent_for_chat` — ANY non-null `agent` value
   * does, with no special case for `"ctrl"`), which would silently strip
   * Concierge's delegation/tool capability. So even though `ctrl` now
   * legitimately appears in `$agentRoster` (role=assistant, hidden=false,
   * per #3812+#3819's composed picker filters), it is explicitly EXCLUDED
   * from the roster-driven "Assistants" list below and represented ONLY by
   * the dedicated `selectConcierge` row — Bob's "Concierge appears exactly
   * once" fix (a prior pass rendered both a hardcoded pinned row AND
   * `ctrl`'s own roster entry).
   * What: Renders the active selection's display name as an `<h1>`-style
   * pane title; a dropdown grouped into TWO typed sections per Bob's
   * roster-typing directive — "System Tool" (Concierge only, hardcoded) and
   * "Assistants" (`$agentRoster` minus `ctrl` — currently Izzie/CTO
   * Assistant; the base `assistant` template never appears here at all,
   * it's `hidden` server-side and only ever instantiated via Concierge or
   * the add-agent template flow, `AddAgentForm`); a "+ Add agent" row; and
   * a gear button toggling the agent-configuration takeover (#3894) for
   * whichever agent is selected. The gear only flips
   * `stores/configPane.ts`'s `configPaneOpen`: the config surface covers the
   * recap rail as well as this column, so it is rendered by `ChatPane` as a
   * SIBLING of the whole chat column rather than mounted here — which is
   * also what lets the chat stay mounted (scroll offset + half-typed
   * message) behind it instead of being unmounted and rebuilt on exit.
   * Test: `ChatPane.test.ts` (the gear opens the takeover). Manual — open the
   * app, confirm "Concierge" is the default title and appears exactly once in
   * the open dropdown (System Tool section); switch to Izzie via the
   * Assistants section, confirm the title updates.
   */
  import { onMount } from 'svelte';
  import { ChevronDown, Settings2, Plus, Bot } from 'lucide-svelte';
  import {
    activeAgentId,
    agentRoster,
    fetchAgentCatalog,
    refreshOverlayAgents,
  } from '../stores/app';
  // `CONCIERGE_AGENT_ID` — the one agent id Concierge's dedicated row
  // represents — is excluded from the roster-driven "Assistants" section to
  // avoid the duplicate. Both constants moved to `lib/roster.ts` in #3894 so
  // the config takeover resolves the same identity from the same place.
  import { CONCIERGE_AGENT_ID, CONCIERGE_LABEL, rosterDisplayName } from '../lib/roster';
  import { configPaneOpen, toggleConfigPane } from '../stores/configPane';
  import AddAgentForm from './AddAgentForm.svelte';

  let open = false;
  let addingAgent = false;

  $: isConcierge = $activeAgentId === null;
  $: title = isConcierge ? CONCIERGE_LABEL : rosterDisplayName($agentRoster, $activeAgentId);
  // #3819 roster-typing: "Assistants" section is everything the roster
  // returns EXCEPT ctrl (Concierge has its own dedicated System Tool row).
  $: assistantEntries = $agentRoster.filter((e) => e.id !== CONCIERGE_AGENT_ID);

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
        <li class="px-3 pt-1.5 pb-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          System Tool
        </li>
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
        <li class="mt-1 border-t border-foundry-light-border dark:border-foundry-border px-3 pt-1.5 pb-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          Assistants
        </li>
        {#each assistantEntries as entry (entry.id)}
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

  <button
    type="button"
    class="rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
    aria-label="Configure agent"
    aria-pressed={$configPaneOpen}
    on:click={toggleConfigPane}
  >
    <Settings2 class="h-4 w-4" />
  </button>
</div>
