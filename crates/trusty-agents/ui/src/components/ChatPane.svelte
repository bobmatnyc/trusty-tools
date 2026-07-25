<script lang="ts">
  /**
   * Why (#3894): the Chat view's composition — chat column + recap rail +
   * conditional Slack mirror — moved out of `App.svelte` because the agent
   * configuration takeover has to be a SIBLING of that whole group (it covers
   * the recap rail, not just the chat column), and because the invariant Bob
   * asked for ("leaving config returns to chat exactly as it was") is a
   * property of this composition that is worth pinning in a test —
   * `App.svelte` itself is not mountable in jsdom (sidecar bootstrap,
   * EventSource, Tauri `invoke`).
   * What: A positioned (`relative`) chat area. While `configPaneOpen` is set,
   * `AgentConfigOverlay` is absolutely positioned over it and the chat group
   * is marked `inert` — hidden and unreachable, but still MOUNTED, which is
   * exactly what preserves the scrollback offset and any half-typed message
   * in the composer. Unmounting it (an `{#if}`/`{:else}` swap) would rebuild
   * both from scratch on exit and silently discard the draft.
   * Test: `ChatPane.test.ts`.
   */
  import ChatHeader from './ChatHeader.svelte';
  import ChatView from './ChatView.svelte';
  import InputArea from './InputArea.svelte';
  import RecapPanel from './RecapPanel.svelte';
  import SlackMirror from './SlackMirror.svelte';
  import AgentConfigOverlay from './AgentConfigOverlay.svelte';
  import { slackMirror } from '../lib/slack-mirror';
  import { configPaneOpen } from '../stores/configPane';
</script>

<div class="relative flex flex-1 min-h-0">
  <!-- #3219: RecapPanel is a persistent right rail (own scroll, AGENTS ACTIVE
       / FILES TOUCHED / TOKENS + folded recap summary), so it sits beside the
       chat+input column rather than stacked between them. -->
  <div class="flex flex-1 min-h-0" data-chat-surface inert={$configPaneOpen}>
    <div class="flex flex-1 flex-col min-w-0">
      <ChatHeader />
      <ChatView />
      <InputArea />
    </div>
    <RecapPanel />
    <!-- #3752: the live Slack mirror only mounts once there's activity, so
         normal (non-Slack) usage is visually unchanged. -->
    {#if $slackMirror.length > 0}
      <SlackMirror />
    {/if}
  </div>

  {#if $configPaneOpen}
    <AgentConfigOverlay />
  {/if}
</div>
