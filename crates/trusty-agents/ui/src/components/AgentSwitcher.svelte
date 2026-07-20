<script lang="ts">
  /**
   * Why: #3223 (Trusty Agents agent roster, epic #3052) — the GUI needs a
   * visible way to pick WHICH named agent (the base assistant, a bundled
   * catalog agent, or a user's personalization overlay) answers the current
   * chat, and to see at a glance who's currently active. A compact dropdown
   * in a slim chat-column header satisfies both "roster UI" and "visibly
   * reflected in the chat header" from the issue in one place, without a
   * full sidebar section competing with Projects/Task History for space.
   * What: Renders the active roster entry as a rectangular, mono-label
   * button; clicking opens a list of every `$agentRoster` entry. Selecting a
   * row sets `activeAgentId` (read by `InputArea.svelte` on next submit) and
   * closes the list. Fetches the roster's two data sources on mount:
   * `fetchAgentCatalog()` (REST, both transports) and
   * `refreshOverlayAgents()` (Tauri-only, no-ops in browser mode).
   * Test: Manual — open the app, confirm "ASSISTANT" renders by default;
   * click it, confirm the roster list opens with every bundled + overlay
   * agent; click a row, confirm the button label updates and the next chat
   * message's `POST /api/task` body carries the new `agent` value.
   */
  import { onMount } from 'svelte';
  import { ChevronDown, Bot } from 'lucide-svelte';
  import {
    activeAgentId,
    agentRoster,
    fetchAgentCatalog,
    refreshOverlayAgents,
  } from '../stores/app';

  let open = false;

  $: activeEntry =
    $agentRoster.find((entry) => entry.id === $activeAgentId) ?? $agentRoster[0];

  function toggle() {
    open = !open;
  }

  function select(id: string) {
    activeAgentId.set(id);
    open = false;
  }

  function handleWindowClick(event: MouseEvent) {
    if (!open) return;
    const target = event.target as HTMLElement;
    if (!target.closest('[data-agent-switcher]')) {
      open = false;
    }
  }

  onMount(() => {
    fetchAgentCatalog().catch((e) => console.error('[AgentSwitcher] fetchAgentCatalog failed:', e));
    refreshOverlayAgents();
  });
</script>

<svelte:window on:click={handleWindowClick} />

<div class="relative inline-block" data-agent-switcher>
  <button
    type="button"
    class="flex items-center gap-1.5 rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-foundry-light-text dark:text-foundry-text hover:border-foundry-light-primary dark:hover:border-foundry-primary"
    on:click={toggle}
    aria-haspopup="listbox"
    aria-expanded={open}
  >
    <Bot class="h-3.5 w-3.5 text-foundry-light-primary dark:text-foundry-primary" />
    {activeEntry?.label ?? 'Assistant'}
    <ChevronDown class="h-3 w-3 opacity-60" />
  </button>

  {#if open}
    <ul
      role="listbox"
      class="absolute left-0 top-full z-30 mt-1 max-h-72 w-56 overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface py-1 shadow-lg"
    >
      {#each $agentRoster as entry (entry.id)}
        <li>
          <button
            type="button"
            role="option"
            aria-selected={entry.id === $activeAgentId}
            class="flex w-full flex-col items-start gap-0.5 px-3 py-1.5 text-left text-xs transition-colors {entry.id ===
            $activeAgentId
              ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
              : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
            on:click={() => select(entry.id)}
          >
            <span class="font-medium">{entry.label}</span>
            <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
              {entry.source === 'base' ? 'base' : entry.source === 'overlay' ? 'your overlay' : 'catalog'}
            </span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>
