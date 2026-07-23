<script lang="ts">
  import { onMount, onDestroy, afterUpdate } from 'svelte';
  import { Loader2 } from 'lucide-svelte';
  import { get } from 'svelte/store';
  import {
    activeMessages,
    activeProjectId,
    agentRoster,
    setMessageSpeakerByTask,
    updateMessageByTask,
    streamDeltaIntoTask,
    isRunning,
  } from '../stores/app';
  import { responderDisplayName } from '../lib/roster';
  import { listenEvent, type UnlistenFn } from '../lib/transport';
  import { streamAccumulator, type DeltaPayload } from '../lib/chatStream';
  import ActionIcon from '../lib/icons/ActionIcon.svelte';
  import WorkflowPhaseCard from './WorkflowPhaseCard.svelte';
  import { workflowState } from '../stores/workflow';

  let scrollEl: HTMLDivElement | undefined;
  let unlistenProgress: UnlistenFn | null = null;
  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let unlistenDelta: UnlistenFn | null = null;

  // Token-streaming accumulator: grows the in-flight reply bubble from
  // `task-delta` fragments and lets the progress handler know which tasks are
  // streaming (so "Running…" ticks don't clobber the live text). Cleared on
  // task completion so the authoritative narrative replaces — not appends to —
  // the accumulation. Shared (module singleton) so `InputArea`'s reconcile
  // handler consults the SAME streaming state and never overwrites live text.
  const streams = streamAccumulator;

  interface ProgressPayload {
    task_id: string;
    message: string;
  }
  interface CompletePayload {
    id: string;
    narrative?: string;
    status?: string;
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
    unlistenProgress = await listenEvent<ProgressPayload>('task-progress', (p) => {
      // Don't let a polling "Running…" tick overwrite live streamed text.
      if (streams.isStreaming(p.task_id)) return;
      updateMessageByTask($activeProjectId, p.task_id, p.message);
    });
    unlistenComplete = await listenEvent<CompletePayload>('task-complete', (p) => {
      // Drop any streamed buffer FIRST so the authoritative narrative replaces
      // (never appends to) the accumulation — the streaming dedupe contract.
      streams.finalize(p.id);
      const text = p.narrative && p.narrative.length > 0 ? p.narrative : '(no narrative)';
      updateMessageByTask($activeProjectId, p.id, text);
      // #3737: if the turn delegated, relabel the bubble to the agent that
      // actually answered (resolved to its display name via the roster).
      const responder = responderDisplayName(get(agentRoster), p.responder_agent);
      if (responder) {
        setMessageSpeakerByTask($activeProjectId, p.id, responder);
      }
      isRunning.set(false);
    });
    unlistenError = await listenEvent<ErrorPayload>('task-error', (p) => {
      streams.finalize(p.task_id);
      updateMessageByTask($activeProjectId, p.task_id, `Error: ${p.error}`);
      isRunning.set(false);
    });
  }

  onMount(() => {
    wireListeners().catch((e) =>
      console.error('[ChatView] wireListeners failed:', e),
    );
  });

  onDestroy(() => {
    unlistenProgress?.();
    unlistenComplete?.();
    unlistenError?.();
    unlistenDelta?.();
  });

  afterUpdate(() => {
    // Why: afterUpdate fires after the DOM is already patched — tick() is
    // redundant here and creates an infinite microtask chain (afterUpdate →
    // tick() resolves → Svelte flushes → afterUpdate → …) that permanently
    // blocks V8's event loop, starving Playwright CDP and other async tasks.
    // What: Scrolls the message list to the bottom after each render.
    // Test: Send a message that fills the chat — the view scrolls to the newest
    // entry without any blank-screen or scroll freeze.
    if (scrollEl) {
      scrollEl.scrollTop = scrollEl.scrollHeight;
    }
  });

  function fmtTime(ts: number): string {
    return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
</script>

<div bind:this={scrollEl} class="flex-1 overflow-y-auto px-6 py-4 bg-foundry-light-bg dark:bg-foundry-bg">
  {#if $activeMessages.length === 0}
    <div class="mt-10 text-center text-sm text-foundry-light-muted dark:text-foundry-text/50 font-sans">
      Start chatting. Messages will appear here.
    </div>
  {/if}

  <div class="mx-auto flex max-w-3xl flex-col gap-3 font-sans">
    {#each $activeMessages as msg (msg.id)}
      {#if msg.role === 'user'}
        <div class="flex justify-end">
          <div class="max-w-[75%] rounded-2xl bg-foundry-light-surface dark:bg-foundry-surface px-4 py-2 text-foundry-light-text dark:text-foundry-text shadow border border-foundry-light-border dark:border-transparent">
            <p class="whitespace-pre-wrap text-sm leading-relaxed">{msg.content}</p>
            <p class="mt-1 text-right text-[10px] text-foundry-light-muted dark:text-foundry-text/50">{fmtTime(msg.timestamp)}</p>
          </div>
        </div>
      {:else if msg.role === 'assistant'}
        <div class="flex justify-start ml-4">
          <div class="max-w-[85%] rounded-r-2xl rounded-bl-2xl border-l-4 border-foundry-teal bg-foundry-teal/10 px-4 py-2 text-foundry-light-text dark:text-foundry-text shadow-sm">
            <!-- #3737: label each assistant bubble with the specific persona
                 that produced it (stamped on the message at send time), never
                 a generic "agent". `tracking-wide` is kept but `uppercase` is
                 dropped so a mixed-case display name ("CTO Assistant") reads
                 as written rather than being flattened to "CTO ASSISTANT". -->
            <div class="mb-1 flex items-center gap-1 text-[10px] font-medium tracking-wide text-foundry-teal/80">
              <ActionIcon name="agent" size={14} />
              <span>{msg.speaker ?? 'Assistant'}</span>
            </div>
            <p class="whitespace-pre-wrap text-sm leading-relaxed">{msg.content || '…'}</p>
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
          <div class="max-w-[85%] rounded-r-2xl rounded-bl-2xl border-l-4 border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary/10 dark:bg-foundry-primary/10 px-4 py-2 text-foundry-light-text dark:text-foundry-text shadow-sm">
            <div class="mb-1 flex items-center gap-1 text-[10px] font-medium uppercase tracking-wide text-foundry-light-primary/80 dark:text-foundry-primary/80">
              <ActionIcon name="pm" size={14} />
              <span>pm</span>
            </div>
            <p class="whitespace-pre-wrap text-sm leading-relaxed">{msg.content || '…'}</p>
            <p class="mt-1 text-[10px] text-foundry-light-primary/80 dark:text-foundry-primary/80">{fmtTime(msg.timestamp)}</p>
          </div>
        </div>
      {:else}
        <div class="flex justify-center">
          <p class="max-w-[75%] text-center text-xs italic text-foundry-light-muted dark:text-foundry-text/50">{msg.content}</p>
        </div>
      {/if}
    {/each}

    {#if $workflowState.phases.length > 0}
      <!-- #3218: inline RESEARCH/PLAN/IMPLEMENT/VERIFY checklist for the
           active task, fed live by the structured workflow store. -->
      <div class="flex justify-start ml-4">
        <div class="max-w-[85%] w-full">
          <WorkflowPhaseCard />
        </div>
      </div>
    {/if}

    {#if $isRunning}
      <div class="flex items-center justify-start gap-2 text-xs text-foundry-teal">
        <Loader2 class="h-3 w-3 animate-spin" />
        <span>Running…</span>
      </div>
    {/if}
  </div>
</div>

<style>
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
