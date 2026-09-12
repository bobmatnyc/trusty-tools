<script lang="ts">
  import ChatAttachments from './ChatAttachments.svelte';
  import { onMount, onDestroy, afterUpdate } from 'svelte';
  import { Loader2 } from 'lucide-svelte';
  import { get } from 'svelte/store';
  import {
    activeMessages,
    recordToolActivity,
    finishToolActivities,
    conversationForTask,
    agentRoster,
    setMessageSpeakerByTask,
    updateMessageByTask,
    streamDeltaIntoTask,
    isRunning,
    activeTaskId,
    canLoadOlderChat,
    loadingOlderChat,
  } from '../stores/app';
  import { loadOlderChat } from '../lib/chatHistory';
  import { responderDisplayName } from '../lib/roster';
  import { listenEvent, type UnlistenFn } from '../lib/transport';
  import { streamAccumulator, type DeltaPayload } from '../lib/chatStream';
  import { isPinnedToBottom } from '../lib/chatScroll';
  import ActionIcon from '../lib/icons/ActionIcon.svelte';
  import ToolActivity from './ToolActivity.svelte';
  import { renderChatMarkdown, assistantEmptyNotice } from '../lib/chatRendering';
  // #7370: a turn's attachments render as cards, and the rendered attachment
  // blocks the model saw are hidden from the bubble — the cards ARE the
  // presentation of that content.
  import AttachmentCard from './AttachmentCard.svelte';
  import { visibleText } from '../lib/attachments';
  import WorkflowPhaseCard from './WorkflowPhaseCard.svelte';
  import { workflowState } from '../stores/workflow';

  let scrollEl: HTMLDivElement | undefined;
  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let unlistenDelta: UnlistenFn | null = null;
  let unlistenTool: UnlistenFn | null = null;
  let destroyed = false;

  // Token-streaming accumulator: grows the in-flight reply bubble from
  // `task-delta` fragments and lets the progress handler know which tasks are
  // streaming (so "Running…" ticks don't clobber the live text). Cleared on
  // task completion so the authoritative narrative replaces — not appends to —
  // the accumulation. Shared (module singleton) so `InputArea`'s reconcile
  // handler consults the SAME streaming state and never overwrites live text.
  const streams = streamAccumulator;

  interface CompletePayload {
    id: string;
    narrative?: string;
    status?: string;
    errors?: string[];
    /**
     * #3737: server-authoritative name of the specialist that actually
     * answered, present only when the turn delegated. When set, it overrides
     * the request-time speaker stamp so the bubble reflects who ANSWERED
     * (e.g. "Izzie") rather than who was asked ("Assistant").
     */
    responder_agent?: string;
  }
  interface ErrorPayload {
    task_id: string;
    error: string;
  }

  /**
   * Why: ChatView listens for backend-emitted Tauri events so a task spawned
   * via `send_message` can stream progress into the same assistant bubble.
   * The InputArea creates a placeholder message tagged with the task id;
   * these handlers find it by id and mutate the content in place.
   * What: Subscribes to the three task events for the lifetime of the view.
   * Test: Send a message, observe the placeholder bubble content grow as
   * `task-progress` events fire, then get replaced by the final narrative on
   * `task-complete`.
   */
  async function wireListeners() {
    unlistenTool = await listenEvent<{ task_id: string; call_id: string; tool: string; status: 'running' | 'complete' | 'error' }>('task-tool-activity', recordToolActivity);
    if (destroyed) { unlistenTool(); return; }
    // Token-level streaming: each fragment grows the in-flight bubble. The
    // fragment's `agent` (when present) keeps per-message attribution (#3739)
    // truthful mid-stream. The terminal `done` marker carries no text — the
    // authoritative `task-complete` narrative is what finally replaces the
    // accumulation, so we do NOT clear the buffer here (that happens on
    // completion) to keep suppressing progress ticks until the real result lands.
    unlistenDelta = await listenEvent<DeltaPayload>('task-delta', (p) => {
      if (p.done) {
        // End-of-stream marker (fires on BOTH success and failure — the
        // backend always emits it, even when it then falls back to a blocking
        // call). Finalize now so we stop suppressing progress ticks: on the
        // fallback path the blocking call's second run then shows "Running…"
        // and the authoritative `task-complete` narrative replaces the stale
        // partial text (never frozen). The accumulated text already lives in
        // the bubble; finalize only drops the internal buffer + streaming flag.
        streams.finalize(p.task_id);
        return;
      }
      // Grow the SINGLE in-flight bubble in place via one store write, keyed by
      // the delta's unique backend id (attributed to its OWNING conversation,
      // not the viewed one). Empty fragments still `append('')` so the task is
      // marked streaming (gating progress ticks). Content + speaker go through a
      // single update: no second render, no mid-stream flash/flicker. Frames
      // that arrive before the poll loop reconciles the bubble's `pending-` id
      // match nothing and are dropped; the accumulator keeps their text, so the
      // first matching write shows everything so far — no lost tokens.
      const full = streams.append(p.task_id, p.text ?? '');
      streamDeltaIntoTask(p.task_id, full, p.agent || undefined);
    });
    // Note: `task-progress` events are intentionally NOT consumed here. Their
    // only former job was writing interim "Running…" status text into the
    // bubble; that text is no longer shown (the spinner is the sole waiting
    // indicator), so the bubble stays empty until the first `task-delta` or the
    // final `task-complete` narrative. The pending→real id reconcile still
    // happens in InputArea's own one-shot progress listener.
    unlistenComplete = await listenEvent<CompletePayload>('task-complete', (p) => {
      // Drop any streamed buffer FIRST so the authoritative narrative replaces
      // (never appends to) the accumulation — the streaming dedupe contract.
      streams.finalize(p.id);
      finishToolActivities(p.id, 'complete');
      const text = p.narrative?.trim() ? p.narrative
        : /cancel/i.test(p.status ?? '') ? 'The request was cancelled without returning a response.'
        : /fail|error/i.test(p.status ?? '') ? 'The request failed without returning a response.' : '';
      const owner = conversationForTask(p.id);
      if (!owner) return;
      // #7370: keep partial-result notices outside the assistant's narrative.
      const notices = p.status === 'partial' && Array.isArray(p.errors)
        ? p.errors.filter((error): error is string => typeof error === 'string' && !!error.trim()) : [];
      updateMessageByTask(owner, p.id, text, notices);
      // #3737: if the turn delegated, relabel the bubble to the agent that
      // actually answered (resolved to its display name via the roster).
      const responder = responderDisplayName(get(agentRoster), p.responder_agent);
      if (responder) {
        setMessageSpeakerByTask(owner, p.id, responder);
      }
      isRunning.set(false);
    });
    unlistenError = await listenEvent<ErrorPayload>('task-error', (p) => {
      streams.finalize(p.task_id);
      finishToolActivities(p.task_id, 'error');
      const owner = conversationForTask(p.task_id);
      if (owner) updateMessageByTask(owner, p.task_id, `Error: ${p.error}`);
      isRunning.set(false);
    });
  }

  onMount(() => {
    wireListeners().catch((e) =>
      console.error('[ChatView] wireListeners failed:', e),
    );
  });

  onDestroy(() => {
    destroyed = true;
    unlistenComplete?.();
    unlistenError?.();
    unlistenDelta?.();
    unlistenTool?.();
  });

  /**
   * Why (PR #3895 code-critic HIGH-3): this used to force
   * `scrollTop = scrollHeight` on EVERY render, unconditionally. That drags a
   * reader who scrolled up to read history back to the newest message the
   * instant a streaming delta arrives — and because the chat keeps rendering
   * while the agent-config takeover covers it (#3894 deliberately leaves it
   * mounted so its scroll offset survives), a delta arriving mid-configuration
   * silently reset the covered chat's position, breaking the very thing the
   * takeover promises. Anchoring to "was the reader already at the bottom?" is
   * the generally-correct chat behavior and fixes both cases at once, without
   * the takeover needing a special case.
   * What: `pinnedToBottom` tracks the reader's intent from actual scroll
   * events (it starts true — an empty chat is at its bottom); `afterUpdate`
   * only re-anchors while that holds.
   * Test: `lib/chatScroll.test.ts` (the predicate) and `ChatPane.test.ts`
   * (a message appended while the takeover is open leaves the covered chat's
   * offset alone).
   */
  let pinnedToBottom = true;

  // #4278: page one more window of persisted history in above what is rendered.
  // `loadOlderChat` reads the armed cursor for the agent, the speaker, AND the
  // bucket, so this passes nothing — a stale cursor cannot be pointed at the
  // wrong conversation from here.
  async function loadEarlier() {
    const result = await loadOlderChat();
    if (result.reason) {
      console.warn('[ChatView] could not load earlier messages:', result.reason);
    }
  }

  function onScroll() {
    if (scrollEl) pinnedToBottom = isPinnedToBottom(scrollEl);
  }

  afterUpdate(() => {
    // Why: afterUpdate fires after the DOM is already patched — tick() is
    // redundant here and creates an infinite microtask chain (afterUpdate →
    // tick() resolves → Svelte flushes → afterUpdate → …) that permanently
    // blocks V8's event loop, starving Playwright CDP and other async tasks.
    // Test: Send a message that fills the chat — the view scrolls to the newest
    // entry without any blank-screen or scroll freeze; scroll up mid-stream and
    // the view stays where you put it.
    if (scrollEl && pinnedToBottom) {
      scrollEl.scrollTop = scrollEl.scrollHeight;
    }
  });

  function fmtTime(ts: number): string {
    return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
</script>

<div
  bind:this={scrollEl}
  on:scroll={onScroll}
  data-chat-scroll
  class="min-w-0 min-h-0 flex-1 overflow-y-auto [overflow-wrap:anywhere] px-6 py-4 bg-foundry-light-bg dark:bg-foundry-bg"
>
  {#if $activeMessages.length === 0}
    <div class="mt-10 text-center text-sm text-foundry-light-muted dark:text-foundry-text/50 font-sans">
      Start chatting. Messages will appear here.
    </div>
  {/if}

  <div class="chat-message-list flex w-full min-w-0 flex-col gap-6 font-sans">
    <!-- #4278: the other half of the bounded initial load. Rehydration seeds
         only the newest page of a `persona-{agent}` session that never rolls
         over, so without this control the bound would just be truncation.
         `canLoadOlderChat` — not the cursor's `hasMore` — is the gate: the
         cursor is global and survives an agent or project switch, and a
         continuous persona session always reports more history, so gating on
         `hasMore` alone left the button offering another agent's turns. -->
    {#if $canLoadOlderChat}
      <div class="flex justify-center">
        <button
          type="button"
          on:click={loadEarlier}
          disabled={$loadingOlderChat}
          class="rounded-full border border-foundry-light-border dark:border-foundry-primary/30 px-3 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/70 hover:text-foundry-light-text dark:hover:text-foundry-text disabled:opacity-50"
        >
          {$loadingOlderChat ? 'Loading…' : 'Load earlier messages'}
        </button>
      </div>
    {/if}
    {#each $activeMessages as msg (msg.id)}
      {#if msg.role === 'user' || msg.role === 'event'}
        <div class="flex w-full justify-end" data-incoming-message>
          <div class="incoming-bubble min-w-0 px-4 py-3 text-foundry-light-text dark:text-foundry-text">
            {#if visibleText(msg.content)}
              <p class="whitespace-pre-wrap break-words text-left text-sm leading-7">{visibleText(msg.content)}</p>
            {/if}
            {#if msg.attachments?.length}
              <div class="flex flex-col" data-attachment-list>
                {#each msg.attachments as attachment (attachment.id)}
                  <AttachmentCard {attachment} />
                {/each}
              </div>
            {/if}
            <ChatAttachments attachments={msg.inlineAttachments ?? []} assistant={msg.attachmentAssistant} />
            <p class="mt-1 text-right text-[10px] text-foundry-light-muted dark:text-foundry-text/50">{fmtTime(msg.timestamp)}</p>
          </div>
        </div>
      {:else if msg.role === 'tool'}
        <ToolActivity name={msg.toolName ?? 'Tool activity'} details={msg.content} status={msg.activityStatus} />
      {:else if msg.role === 'assistant'}
        <!-- Why: the waiting indicator belongs to the bubble it is waiting on,
             so BOTH the in-bubble spinner and the pulsing green border read
             from this one expression — they cannot disagree. `isRunning`
             (stores/app.ts) is the authoritative "waiting on the agent" flag;
             `activeTaskId` (same store, written in the same tick by
             InputArea's submit) says WHICH bubble it belongs to. Both are
             cleared on every terminal path — `task-complete` and `task-error`
             above, and `session_cancelled` reaches the latter via
             `lib/eventBridge.ts` — so a failed or cancelled task never leaves
             a bubble pulsing.
             Test: `ChatView.test.ts` (the spinner appears/clears with the
             flag, inside the bubble). -->
        {@const waiting = $isRunning && !!msg.taskId && msg.taskId === $activeTaskId}
        <div class="flex w-full">
          <div
            class="w-full min-w-0 px-1 py-2 text-foundry-light-text dark:text-foundry-text {waiting ? 'waiting-bubble' : ''}"
          >
            <!-- #3737: label each assistant bubble with the specific persona
                 that produced it (stamped on the message at send time), never
                 a generic "agent". `tracking-wide` is kept but `uppercase` is
                 dropped so a mixed-case display name ("CTO Assistant") reads
                 as written rather than being flattened to "CTO ASSISTANT". -->
            <div class="mb-1 flex items-center gap-1 text-[10px] font-medium tracking-wide text-foundry-teal/80">
              <ActionIcon name="agent" size={14} />
              <span>{msg.speaker ?? 'Assistant'}</span>
            </div>
            <!-- The spinner lives in the BODY, where the `…` placeholder used
                 to be: while the reply is still empty it IS the body (so the
                 bubble reads as "this message is being written" rather than
                 showing a dead ellipsis next to a detached spinner), and once
                 streamed text arrives it trails the last token like a caret.
                 Kept on one source line — `whitespace-pre-wrap` would render
                 the markup's own indentation as literal text. `role="status"`
                 + `aria-label` carry the accessible busy name that the removed
                 out-of-bubble element used to own. -->
            <div data-assistant-body class="assistant-markdown break-words text-sm leading-7">{#if msg.content.trim()}{@html renderChatMarkdown(msg.content)}{:else if !waiting}<p>{assistantEmptyNotice(msg, $activeMessages)}</p>{/if}{#if waiting}<span class="ml-0.5 inline-flex align-middle text-foundry-light-success dark:text-foundry-success" role="status" aria-label="Assistant is responding"><Loader2 class="h-3 w-3 animate-spin" aria-hidden="true" /></span>{/if}</div>
            <p class="mt-1 text-[10px] text-foundry-teal/70">{fmtTime(msg.timestamp)}</p>
          </div>
        </div>
      {:else if msg.role === 'recap'}
        <!-- Why: #371 recap messages render as a distinctive teal-bordered
             banner with a step/result table, so users can scan what changed
             since the last recap without leaving the chat. -->
        <div class="flex justify-center">
          <div
            class="w-full max-w-[95%] rounded-lg border border-foundry-teal/30 bg-foundry-teal/5 dark:bg-foundry-teal/10 px-3 py-2 my-2 font-mono text-xs"
          >
            <div class="mb-1 flex items-center gap-2 text-foundry-teal">
              <span aria-hidden="true">※</span>
              <span class="font-semibold uppercase tracking-wide">recap</span>
              <span class="text-foundry-light-text/70 dark:text-foundry-text/70 truncate">
                · {msg.content}
              </span>
              <span class="ml-auto text-[10px] text-foundry-teal/60">{fmtTime(msg.timestamp)}</span>
            </div>
            {#if msg.recapRows && msg.recapRows.length > 0}
              <table class="w-full border-collapse">
                <thead>
                  <tr class="text-foundry-teal/70 border-b border-foundry-teal/20">
                    <th class="text-left py-1 pr-4 w-32 font-normal">Step</th>
                    <th class="text-left py-1 font-normal">Result</th>
                  </tr>
                </thead>
                <tbody>
                  {#each msg.recapRows as [step, result], i (i)}
                    <tr class="border-b border-foundry-teal/10 last:border-0 recap-row">
                      <td class="py-0.5 pr-4 text-foundry-teal/80 whitespace-nowrap align-top">
                        {step}
                      </td>
                      <td class="py-0.5 text-foundry-light-text/80 dark:text-foundry-text/70 break-words">
                        {result}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
          </div>
        </div>
      {:else if msg.role === 'pm'}
        <div class="flex justify-start">
          <div class="w-full min-w-0 px-1 py-2 text-foundry-light-text dark:text-foundry-text">
            <div class="mb-1 flex items-center gap-1 text-[10px] font-medium uppercase tracking-wide text-foundry-light-primary/80 dark:text-foundry-primary/80">
              <ActionIcon name="pm" size={14} />
              <span>pm</span>
            </div>
            <p class="whitespace-pre-wrap break-words text-sm leading-7">{msg.content || '…'}</p>
            <p class="mt-1 text-[10px] text-foundry-light-primary/80 dark:text-foundry-primary/80">{fmtTime(msg.timestamp)}</p>
          </div>
        </div>
      {:else if msg.role === 'topic-boundary'}
        <!-- #3819: "+ New Task" inserts this divider instead of clearing
             context — one continuous chat per agent, topic boundaries are a
             marker in the stream, not a wall. -->
        <div class="my-2 flex items-center gap-3" role="separator">
          <span class="h-px flex-1 bg-foundry-light-border dark:bg-foundry-border"></span>
          <span class="font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
            {msg.content || 'New task'} · {fmtTime(msg.timestamp)}
          </span>
          <span class="h-px flex-1 bg-foundry-light-border dark:bg-foundry-border"></span>
        </div>
      {:else}
        <div class="flex justify-center">
          <p class="max-w-[75%] text-center text-xs italic text-foundry-light-muted dark:text-foundry-text/50">{msg.content}</p>
        </div>
      {/if}
      {#if msg.hostNotices?.length}
        <div data-host-notice role="status" class="rounded-lg border border-foundry-amber/40 bg-foundry-amber/10 px-3 py-2 text-sm text-foundry-light-text dark:text-foundry-text">
          <p class="mb-1 font-medium">Trusty Agents notice</p>
          {#each msg.hostNotices as notice}
            <p class="whitespace-pre-wrap break-words">{notice}</p>
          {/each}
        </div>
      {/if}
    {/each}

    {#if $workflowState.phases.length > 0}
      <!-- #3218: inline RESEARCH/PLAN/IMPLEMENT/VERIFY checklist for the
           active task, fed live by the structured workflow store. -->
      <div class="flex w-full">
        <div class="w-full">
          <WorkflowPhaseCard />
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .assistant-markdown :global(p) { margin:0 0 .65em; white-space:pre-wrap; }
  .assistant-markdown :global(pre) { overflow:auto; padding:12px; border-radius:8px; background:rgb(var(--color-text-primary) / .05); }
  .assistant-markdown :global(code) { font-size:.9em; white-space:pre-wrap; overflow-wrap:anywhere; }
  .assistant-markdown :global(ul), .assistant-markdown :global(ol) { padding-left:1.5em; margin:.6em 0; }
  .assistant-markdown :global(ul) { list-style:disc; }
  .assistant-markdown :global(ol) { list-style:decimal; }
  .assistant-markdown :global(h1), .assistant-markdown :global(h2), .assistant-markdown :global(h3) { font-weight:600; margin:.8em 0 .4em; }
  .assistant-markdown :global(blockquote) { border-left:2px solid rgb(var(--color-border)); padding-left:12px; }
  .assistant-markdown :global(table) { display:block; max-width:100%; overflow:auto; border-collapse:collapse; }
  .assistant-markdown :global(td), .assistant-markdown :global(th) { padding:4px 8px; border:1px solid rgb(var(--color-border)); }

  .chat-message-list :global([data-tool-activity] + [data-tool-activity]) { margin-top:-16px; }
  .incoming-bubble { width:fit-content; max-width:70%; border-radius:16px; background:rgb(128 128 128 / .10); text-align:left; }
  :global(.dark) .incoming-bubble { background:rgb(190 190 190 / .12); }

  /* Why (#3387): was an unsanctioned raw `rgb(20 184 166 / 0.04)` teal-500
     literal with no token relationship (audit finding B2). Now reads
     --trusty-surface-hover (app.css), the DS's own sanctioned hover/tint
     token — same rust-derived hue family as the rest of the app, and
     correctly light/dark-reactive. Alternate-row striping itself (nth-child)
     is kept as-is; the DS's "no zebra-striped tables" guardrail is a
     structural change out of scope for this token-layer pass — flagged as a
     follow-up, not fixed here.
     Test: Inspect a recap message and verify even rows have a faint tint. */
  tbody tr.recap-row:nth-child(even) {
    background-color: var(--trusty-surface-hover);
  }

</style>
