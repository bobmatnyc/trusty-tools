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
   * panel's own Back/Close buttons — all three through
   * `requestExitConfigPane`, which stops to confirm when the panel holds
   * unsaved edits.
   * Test: `ChatPane.test.ts` (takeover mounts over a preserved chat, Esc and
   * Back both exit) and `AgentConfigPanel.test.ts` (the exit affordances
   * themselves).
   */
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { activeAgentId, agentRoster } from '../stores/app';
  import { CONCIERGE_LABEL, configAgentName, rosterDisplayName } from '../lib/roster';
  import { closeConfigPane, isConfigExitKey, requestExitConfigPane } from '../stores/configPane';
  import AgentConfigPanel from './AgentConfigPanel.svelte';

  let container: HTMLElement | null = null;

  /**
   * Why (PR #3895 code-critic HIGH-2): "we are configuring THIS agent" — the
   * identity is fixed the moment the takeover opens, NOT followed reactively
   * from `activeAgentId`. Surfaces outside the covered chat can still write
   * that store (the Sidebar's resume-workstream flow does), and following it
   * would silently retarget an open panel at a different agent's config,
   * throwing away whatever the user had typed into it.
   * What: Read once at mount — this component is `{#if}`-mounted per opening,
   * so mount time IS open time. Only the human-facing LABEL stays reactive,
   * since the roster can finish loading after the pane is already open.
   */
  const pinnedAgentId = get(activeAgentId);
  const isConcierge = pinnedAgentId === null;
  const agentName = configAgentName(pinnedAgentId);
  $: displayName = isConcierge
    ? CONCIERGE_LABEL
    : rosterDisplayName($agentRoster, pinnedAgentId);

  function onKeydown(event: KeyboardEvent) {
    // Carry-forward (PR #3895 review): this does not consult
    // `event.defaultPrevented`. Nothing inside the panel currently handles Esc
    // itself, but the day a dropdown/combobox lands in there, its Esc must
    // close the dropdown rather than the whole takeover — add the check then.
    if (!isConfigExitKey(event)) return;
    event.preventDefault();
    // Never closes directly: unsaved edits raise the panel's confirm instead.
    requestExitConfigPane();
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
