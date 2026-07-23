<script lang="ts">
  /**
   * Why: #3752 (epic #3052) — when the CTO Bot converses on Slack (`tagent
   * --slack`), the GUI mirrors BOTH sides live so an operator watching the
   * desktop app sees the inbound human message and the bot's reply as they
   * happen. The bubbles + honest badges come from the `slackMirror` store,
   * which App.svelte's SSE bridge feeds from the two `slack_message_received`
   * / `slack_reply_sent` events on the existing `/api/events` stream.
   * What: A right-hand `<aside>` (own vertical scroll, Foundry styling to
   * match RecapPanel) listing conversation bubbles: inbound = left, with the
   * sender name + honest RBAC-tier badge; reply = right, with the honest bot
   * identity badge (`CTO Bot (as itself)` — there is NO impersonation mode).
   * The parent only mounts this when the store is non-empty, so normal usage
   * with no Slack activity is unaffected.
   * Test: `slack-mirror.test.ts` covers the event→bubble transform + store
   * fold this renders; live render is exercised via the runbook (PR body).
   */
  import { slackMirror, clearSlackMirror, type SlackBubble } from '../lib/slack-mirror';

  // Truncate a Slack channel id for the compact header/meta line.
  function shortChannel(c: string): string {
    return c ? `#${c}` : '#—';
  }

  function badgeClass(b: SlackBubble): string {
    // Reply badge uses the teal bot-identity accent; inbound tier badge uses
    // the neutral surface chip so access level reads as metadata, not status.
    return b.kind === 'reply'
      ? 'bg-foundry-teal/15 text-foundry-teal'
      : 'bg-foundry-light-border/60 dark:bg-black/40 text-foundry-light-muted dark:text-foundry-text/60';
  }
</script>

<aside
  class="flex w-[320px] flex-shrink-0 flex-col border-l border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface overflow-hidden"
>
  <div
    class="flex items-center gap-2 px-4 py-3 border-b border-foundry-light-border dark:border-foundry-border font-mono text-[10px] font-semibold uppercase tracking-widest text-foundry-light-muted dark:text-foundry-text/60"
  >
    <span class="h-1.5 w-1.5 rounded-full bg-foundry-teal animate-pulse" aria-hidden="true"></span>
    <span>Slack Live</span>
    <button
      type="button"
      class="ml-auto text-foundry-light-muted/70 dark:text-foundry-text/40 hover:text-foundry-teal normal-case tracking-normal"
      on:click={clearSlackMirror}
      title="Clear the mirror"
    >
      clear
    </button>
  </div>

  <div class="flex flex-1 flex-col gap-3 overflow-y-auto px-3 py-3">
    {#if $slackMirror.length === 0}
      <p class="px-1 text-xs font-mono text-foundry-light-muted dark:text-foundry-text/40">
        Waiting for Slack activity…
      </p>
    {:else}
      {#each $slackMirror as b, i (b.received_at + '-' + i)}
        <div class="flex flex-col {b.kind === 'reply' ? 'items-end' : 'items-start'}">
          <div class="mb-0.5 flex items-center gap-1.5 px-1 text-[10px] font-mono">
            {#if b.kind === 'inbound'}
              <span class="font-semibold text-foundry-light-text dark:text-foundry-text">{b.speaker || 'unknown'}</span>
            {:else}
              <span class="font-semibold text-foundry-teal">{b.badge}</span>
            {/if}
            {#if b.kind === 'inbound'}
              <span class="rounded px-1 py-px uppercase tracking-wide {badgeClass(b)}" title="RBAC tier">{b.badge}</span>
            {/if}
            <span class="text-foundry-light-muted dark:text-foundry-text/40" title={b.channel}>{shortChannel(b.channel)}</span>
          </div>
          <div
            class="max-w-[85%] whitespace-pre-wrap break-words rounded-lg px-3 py-2 text-sm {b.kind === 'reply'
              ? 'bg-foundry-teal/10 text-foundry-light-text dark:text-foundry-text rounded-tr-sm'
              : 'bg-foundry-light-bg dark:bg-foundry-bg text-foundry-light-text dark:text-foundry-text rounded-tl-sm border border-foundry-light-border dark:border-foundry-border'}"
          >
            {b.text}
          </div>
        </div>
      {/each}
    {/if}
  </div>
</aside>
