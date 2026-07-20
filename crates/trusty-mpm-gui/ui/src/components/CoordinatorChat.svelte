<script lang="ts">
  // Why: The coordinator chat is the permanent main panel of the GUI/WUI —
  // the user talks to the coordinator here to ask questions or route a
  // message to a specific session via an `@session-name:` prefix.
  // What: A scrollable transcript bound to the `chatHistory` store plus an
  // input bar; on send it appends the user turn, POSTs to `coordinator_chat`,
  // shows a typing indicator, then appends the coordinator's reply. On mount
  // it fetches `coordinator_context` and prepends a greeting.
  // Test: Mount with the daemon up → a greeting message listing active
  // sessions appears; type a message and press Enter → the user bubble shows,
  // a typing indicator briefly appears, then a coordinator reply is appended.
  import { onMount, tick } from 'svelte';
  import { Bot, Send } from 'lucide-svelte';
  import { coordinatorChat, coordinatorContext } from '../lib/transport';
  import {
    chatHistory,
    coordinatorContext as contextStore,
    type ChatMessage,
  } from '../stores/app';

  /** The pending input text. */
  let draft = '';

  /** True while awaiting a coordinator reply (drives the typing indicator). */
  let sending = false;

  /** The scrollable transcript element, for auto-scroll. */
  let scrollEl: HTMLDivElement | undefined;

  /**
   * Why: New messages must always be visible without manual scrolling.
   * What: Waits for the DOM to settle, then pins the transcript to the bottom.
   * Test: Append a message and assert `scrollTop` equals `scrollHeight`.
   */
  async function scrollToBottom(): Promise<void> {
    await tick();
    if (scrollEl) scrollEl.scrollTop = scrollEl.scrollHeight;
  }

  // Auto-scroll whenever the transcript or the typing indicator changes.
  $: if ($chatHistory || sending) scrollToBottom();

  /**
   * Why: A friendly opening turn tells the user what the coordinator can see.
   * What: Reads the context payload's session list (tolerating several
   * shapes) and returns a one-line summary.
   * Test: With two active sessions, the greeting names both ids.
   */
  function greetingFrom(ctx: any): string {
    const list: any[] = Array.isArray(ctx)
      ? ctx
      : Array.isArray(ctx?.sessions)
        ? ctx.sessions
        : Array.isArray(ctx?.active_sessions)
          ? ctx.active_sessions
          : [];
    if (list.length === 0) {
      return 'Coordinator ready. No active sessions yet — ask me anything, or start a session.';
    }
    const ids = list
      .map((s) => (typeof s === 'string' ? s : (s?.id ?? s?.name ?? '?')))
      .join(', ');
    return `Coordinator ready. Active sessions: ${ids}. Prefix a message with @session-name: to route it there.`;
  }

  /**
   * Why: The user needs orientation the moment the panel opens.
   * What: Fetches the coordinator context and, if the transcript is empty,
   * prepends a coordinator greeting message.
   * Test: Mount with the daemon up → `chatHistory` gains one coordinator turn.
   */
  onMount(async () => {
    try {
      const ctx = await coordinatorContext();
      contextStore.set(ctx);
      chatHistory.update((h) =>
        h.length > 0
          ? h
          : [
              {
                role: 'coordinator',
                content: greetingFrom(ctx),
                timestamp: new Date(),
              },
            ],
      );
    } catch {
      chatHistory.update((h) =>
        h.length > 0
          ? h
          : [
              {
                role: 'coordinator',
                content: 'Coordinator unreachable — is the daemon running?',
                timestamp: new Date(),
              },
            ],
      );
    }
  });

  /**
   * Why: Sending is the core interaction — append the user turn, call the
   * daemon, then append whatever the coordinator replies.
   * What: Trims the draft, pushes a `user` message (carrying `routed_to` when
   * an `@id:` prefix is present), POSTs to `coordinator_chat`, and appends the
   * normalized `coordinator` reply. Failures append an error turn.
   * Test: Send "hello" → two turns are added (user, coordinator); send
   * "@foo: ls" → the user turn carries `routed_to: 'foo'`.
   */
  async function send(): Promise<void> {
    const text = draft.trim();
    if (!text || sending) return;

    const match = text.match(/^@([\w.\-]+):\s*([\s\S]*)$/);
    const userMsg: ChatMessage = {
      role: 'user',
      content: text,
      timestamp: new Date(),
      ...(match ? { routed_to: match[1] } : {}),
    };

    let history: ChatMessage[] = [];
    chatHistory.update((h) => {
      history = h;
      return [...h, userMsg];
    });
    draft = '';
    sending = true;

    try {
      const reply = await coordinatorChat(text, history);
      const replyMsg: ChatMessage = {
        role: 'coordinator',
        content:
          typeof reply === 'string'
            ? reply
            : (reply?.content ?? reply?.message ?? reply?.reply ?? ''),
        timestamp: new Date(),
        ...(reply?.routed_to ? { routed_to: reply.routed_to } : {}),
        ...(reply?.command_output
          ? { command_output: reply.command_output }
          : {}),
      };
      chatHistory.update((h) => [...h, replyMsg]);
    } catch (err) {
      chatHistory.update((h) => [
        ...h,
        {
          role: 'coordinator',
          content: `Error: ${err instanceof Error ? err.message : String(err)}`,
          timestamp: new Date(),
        },
      ]);
    } finally {
      sending = false;
    }
  }

  /**
   * Why: Enter sends, Shift+Enter inserts a newline — the expected chat UX.
   * What: Intercepts Enter without Shift to call `send()`.
   * Test: Press Enter → `send` runs; press Shift+Enter → a newline is added.
   */
  function onKeydown(ev: KeyboardEvent): void {
    if (ev.key === 'Enter' && !ev.shiftKey) {
      ev.preventDefault();
      send();
    }
  }

  /** Short HH:MM timestamp for a message. */
  function fmtTime(d: Date): string {
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
</script>

<section class="flex h-full min-h-0 flex-col">
  <!-- Transcript -->
  <div
    bind:this={scrollEl}
    class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4"
  >
    {#each $chatHistory as msg, i (i)}
      {#if msg.role === 'coordinator'}
        <div class="flex max-w-[80%] items-start gap-2 self-start">
          <span
            class="mt-0.5 shrink-0 rounded-full bg-trusty-border p-1 text-trusty-primary"
          >
            <Bot size={14} />
          </span>
          <div
            class="rounded-lg bg-trusty-surface px-3 py-2 text-sm"
          >
            <p class="whitespace-pre-wrap break-words">{msg.content}</p>
            {#if msg.command_output}
              <details
                class="mt-2 rounded border border-trusty-border"
              >
                <summary
                  class="cursor-pointer select-none px-2 py-1 text-xs opacity-70"
                >
                  command output
                </summary>
                <pre
                  class="overflow-x-auto px-2 py-1 font-mono text-[11px] leading-snug opacity-90"><code
                    >{msg.command_output}</code
                  ></pre>
              </details>
            {/if}
            <span class="mt-1 block text-[10px] opacity-40">
              {fmtTime(msg.timestamp)}
            </span>
          </div>
        </div>
      {:else}
        <div class="flex max-w-[80%] flex-col items-end gap-1 self-end">
          {#if msg.routed_to}
            <span
              class="rounded-full bg-trusty-primary/15 px-2 py-0.5 font-mono text-[10px] text-trusty-primary"
            >
              → {msg.routed_to}
            </span>
          {/if}
          <div
            class="rounded-lg bg-trusty-primary px-3 py-2 text-sm text-white"
          >
            <p class="whitespace-pre-wrap break-words">{msg.content}</p>
            <span class="mt-1 block text-[10px] opacity-60">
              {fmtTime(msg.timestamp)}
            </span>
          </div>
        </div>
      {/if}
    {/each}

    {#if sending}
      <div class="flex items-center gap-2 self-start">
        <span
          class="shrink-0 rounded-full bg-trusty-border p-1 text-trusty-primary"
        >
          <Bot size={14} />
        </span>
        <div
          class="flex gap-1 rounded-lg bg-trusty-surface px-3 py-2.5"
        >
          <span class="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-40"></span>
          <span
            class="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-40"
            style="animation-delay:0.15s"
          ></span>
          <span
            class="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-40"
            style="animation-delay:0.3s"
          ></span>
        </div>
      </div>
    {/if}
  </div>

  <!-- Input bar -->
  <div
    class="flex items-end gap-2 border-t border-trusty-border bg-trusty-surface p-3"
  >
    <textarea
      bind:value={draft}
      on:keydown={onKeydown}
      rows="1"
      placeholder="@session-name: message or just ask anything..."
      class="max-h-32 min-h-[2.5rem] flex-1 resize-none rounded-md border border-trusty-border bg-trusty-card px-3 py-2 text-sm outline-none focus:border-trusty-primary"
    ></textarea>
    <button
      type="button"
      on:click={send}
      disabled={sending || draft.trim().length === 0}
      aria-label="Send"
      class="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-trusty-primary text-white disabled:opacity-40"
    >
      <Send size={16} />
    </button>
  </div>
</section>
