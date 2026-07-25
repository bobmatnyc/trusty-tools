<script lang="ts">
  /**
   * Why (#3894, epic #3052 — Bob: "when we're in agent configuration, let's
   * take over that pane, we don't need chat when we're configuring"): the
   * agent–OKG–tool–listener config surface is the concept centerpiece, and
   * #3826 gave it a 320px strip wedged above the chat scrollback. This is the
   * takeover layer that gives it the whole content area instead. It is
   * deliberately a POSITIONING + EXIT shell only — every field, tab and save
   * flow still lives in `AgentConfigPanel`, unchanged.
   * What: An absolutely-positioned layer filling its (relative) parent — the
   * chat area in `ChatPane` — so the chat column underneath stays MOUNTED and
   * laid out rather than being `{#if}`-swapped away. That is the whole trick
   * behind "leaving config returns to chat exactly as it was": an unmounted
   * `ChatView`/`InputArea` would lose its scroll offset and the user's
   * half-typed message; a covered one loses nothing. `ChatPane` marks the
   * covered column `inert`, so nothing behind this layer is focusable or
   * reachable by a screen reader. Esc exits (see `isConfigExitKey`), as do the
   * panel's own Back/Close buttons.
   * Test: `ChatPane.test.ts` (takeover mounts over a preserved chat, Esc and
   * Back both exit) and `AgentConfigPanel.test.ts` (the exit affordances
   * themselves).
   */
  import { onMount } from 'svelte';
  import { activeAgentId, agentRoster } from '../stores/app';
  import { CONCIERGE_LABEL, configAgentName, rosterDisplayName } from '../lib/roster';
  import { closeConfigPane, isConfigExitKey } from '../stores/configPane';
  import AgentConfigPanel from './AgentConfigPanel.svelte';

  let container: HTMLElement | null = null;

  $: isConcierge = $activeAgentId === null;
  $: agentName = configAgentName($activeAgentId);
  $: displayName = isConcierge
    ? CONCIERGE_LABEL
    : rosterDisplayName($agentRoster, $activeAgentId);

  function onKeydown(event: KeyboardEvent) {
    if (!isConfigExitKey(event)) return;
    event.preventDefault();
    closeConfigPane();
  }

  // Move focus into the takeover on open so a keyboard user isn't left with
  // focus on the (now `inert`) gear button behind it.
  onMount(() => {
    container?.focus();
  });
</script>

<svelte:window on:keydown={onKeydown} />

<section
  bind:this={container}
  tabindex="-1"
  aria-label="Agent configuration"
  data-config-takeover
  class="absolute inset-0 z-20 flex flex-col bg-foundry-light-surface dark:bg-foundry-surface focus:outline-none"
>
  <AgentConfigPanel {agentName} {isConcierge} {displayName} onExit={closeConfigPane} />
</section>
