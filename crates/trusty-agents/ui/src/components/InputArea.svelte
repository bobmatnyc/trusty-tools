<script lang="ts">
  import { Send, Square } from 'lucide-svelte';
  import {
    activeMessages,
    activeProject,
    activeProjectId,
    activeTaskId,
    addMessage,
    isRunning,
    replaceMessageTaskId,
    setProjectStatus,
    updateMessageByTask,
    type Message,
    type Project,
  } from '../stores/app';
  import { cancelTask, invoke, listenEvent } from '../lib/transport';

  let input = '';
  let textareaEl: HTMLTextAreaElement;
  let cancelling = false;

  $: disabled = !input.trim();

  // Why (#3063): a retask can start before the previous submission's
  // `send_message` invoke/poll-loop has resolved (it only notices the abort
  // on its next ~1.5s tick). Without a guard, that stale call's `finally`
  // block would flip `isRunning`/`activeTaskId` back to "idle" right after
  // the NEW task starts. Every state-mutating callback inside `submitTask`
  // checks its captured `mySeq` against this counter before writing global
  // state, so only the most recent submission's callbacks take effect.
  let submissionSeq = 0;

  /**
   * Why: Retasking (#3063) aborts the in-flight run and starts a brand-new
   * agent invocation — there is no mid-flight message-injection channel (see
   * `cancel.rs`'s design note), so continuity has to be rebuilt by hand. The
   * PM-locked design is "abort + resubmit with history": we fold the visible
   * conversation (already held client-side in the `messages` store) into the
   * new task text so the fresh invocation isn't starting from a blank slate.
   * What: Renders prior user/assistant turns as a plain transcript, flags
   * that the previous task was interrupted, then appends the new
   * instruction. Falls back to the bare instruction when there's no prior
   * history. This only affects the payload sent to the backend — the chat
   * bubble still shows just what the user typed (via `addMessage` in
   * `submitTask`, called with the unmodified `content`).
   * Test: Manual — start a task, retask mid-run, observe the new run's
   * narrative reflects awareness of the earlier turn.
   */
  function buildRetaskPayload(history: Message[], newContent: string): string {
    const turns = history
      .filter((m) => m.role === 'user' || m.role === 'assistant')
      .filter((m) => m.content.trim().length > 0)
      .map((m) => `${m.role === 'user' ? 'User' : 'Assistant'}: ${m.content}`);
    if (turns.length === 0) return newContent;
    return `${turns.join('\n')}\n\n[The previous task was stopped before completion.]\nUser: ${newContent}`;
  }

  /**
   * Why: Core submission logic shared by a normal send and a post-retask
   * resend. Separated from `handleSubmit` so the retask path can send a
   * history-augmented `payloadTask` to the backend while still showing the
   * user's plain `displayContent` in the chat bubble.
   * What: Creates the user + assistant-placeholder messages, calls
   * `invoke('send_message', { content: payloadTask, ... })`, and reconciles
   * the placeholder task id with the real one once the first progress event
   * arrives. Every callback is guarded by `mySeq` (see `submissionSeq` above)
   * so a superseded (retasked-away) submission can't clobber the new one's
   * state.
   * Test: Type "hello", press Enter — a user bubble appears, then an
   * assistant bubble filling with progress, then the final narrative.
   */
  async function submitTask(
    project: Project,
    displayContent: string,
    payloadTask: string,
  ): Promise<void> {
    const mySeq = ++submissionSeq;
    const projectId = project.id;
    const now = Date.now();

    addMessage(projectId, {
      id: `user-${now}`,
      role: 'user',
      content: displayContent,
      timestamp: now,
    });

    // Assistant placeholder. `taskId` is set to a temp id and patched once
    // the backend returns the real id via the resolved promise OR the first
    // progress event (whichever arrives first).
    const placeholderTaskId = `pending-${now}`;
    addMessage(projectId, {
      id: `asst-${now}`,
      role: 'assistant',
      content: '',
      timestamp: now,
      taskId: placeholderTaskId,
    });

    isRunning.set(true);
    activeTaskId.set(placeholderTaskId);
    setProjectStatus(projectId, 'running');

    // Why: `send_message` only resolves with the real task id at the END of
    // the run. In the meantime, `task-progress` events fire with the real
    // backend id — but the placeholder bubble is tagged with `pending-<ts>`,
    // so `updateMessageByTask` would never match and progress would silently
    // drop. We attach a one-shot listener that catches the first progress
    // event for THIS submission and swaps the placeholder id for the real
    // one, after which subsequent progress events route correctly.
    let reconciled = false;
    let unlistenReconcile: (() => void) | null = null;
    const unlistenP = await listenEvent<{ task_id: string; message: string }>(
      'task-progress',
      (p) => {
        if (reconciled || !p.task_id || mySeq !== submissionSeq) return;
        reconciled = true;
        replaceMessageTaskId(projectId, placeholderTaskId, p.task_id);
        // Apply the message that triggered the swap so it isn't lost.
        updateMessageByTask(projectId, p.task_id, p.message);
        activeTaskId.set(p.task_id);
        unlistenReconcile?.();
      },
    );
    unlistenReconcile = unlistenP;

    try {
      const result = await invoke<string>('send_message', {
        content: payloadTask,
        projectPath: project.path ?? null,
      });
      if (mySeq !== submissionSeq) return; // superseded by a retask
      // When `send_message` resolves (Tauri mode), the complete event should
      // already have updated the bubble. If not (browser fallback), we apply
      // the returned narrative directly.
      if (typeof result === 'string' && result.length > 0) {
        updateMessageByTask(projectId, placeholderTaskId, result);
      }
      setProjectStatus(projectId, 'idle');
    } catch (e) {
      if (mySeq !== submissionSeq) return;
      updateMessageByTask(projectId, placeholderTaskId, `Error: ${e}`);
      setProjectStatus(projectId, 'error');
    } finally {
      // Detach reconcile listener if it never fired (e.g. error before any
      // progress event); leaking listeners across submissions would compound.
      unlistenReconcile?.();
      if (mySeq === submissionSeq) {
        isRunning.set(false);
        activeTaskId.set(null);
      }
    }
  }

  /**
   * Why: Entry point for both a normal send and a retask. When no task is
   * running this is a plain submit. When one IS running, submitting is
   * ambiguous — the PM-locked design (#3063) is "abort the running task and
   * resubmit the new instruction with history", so we confirm (this is
   * destructive to the in-flight run) before cancelling and resending.
   * What: Guards on `$isRunning`; the retask branch cancels the active task
   * (best-effort — a failure here just means we proceed anyway, since the
   * user's intent is clearly to move on) then calls `submitTask` with a
   * history-augmented payload. The normal branch calls `submitTask` with the
   * bare content.
   * Test: Type + Enter while idle — normal send. Type + Enter while running —
   * confirm dialog appears; accepting cancels the old task and starts the
   * new one; declining leaves the running task untouched and keeps the input.
   */
  async function handleSubmit() {
    const content = input.trim();
    if (!content) return;

    const project = $activeProject;

    if ($isRunning) {
      const proceed = confirm(
        'A task is still running. Stop it and send this message instead?',
      );
      if (!proceed) return;

      const runningId = $activeTaskId;
      input = '';
      if (runningId) {
        try {
          await cancelTask(runningId);
        } catch (e) {
          // Best-effort: proceed with the resubmit regardless — the user's
          // intent (move on to the new instruction) still stands even if the
          // cancel call itself failed (e.g. transient network error).
          console.error('[InputArea] retask: cancelTask failed, continuing anyway:', e);
        }
      }
      const payload = buildRetaskPayload($activeMessages, content);
      await submitTask(project, content, payload);
      return;
    }

    input = '';
    await submitTask(project, content, content);
  }

  /**
   * Why: The Stop control for #3063 — lets the user abort a runaway or
   * no-longer-wanted task without waiting for it to finish. Deliberately
   * thin: it only fires the cancel request. `submitTask`'s own `finally`
   * block (see above) is what flips `isRunning`/`activeTaskId` back to idle,
   * once the aborted run's poll loop (Tauri: Rust; browser: `fetchFallback`)
   * observes the resulting `status: "cancelled"` on its next tick — bounded
   * by the 1.5s poll interval in both transports.
   * What: Calls `cancelTask`; 404 (already gone) and 409 (already terminal)
   * are both treated as success-adjacent per the backend contract — no error
   * toast, since the aborted run's own terminal-state handling in
   * `submitTask` reconciles the UI regardless. Only a genuine transport
   * failure is logged.
   * Test: Manual — start a long task, click Stop, observe the input
   * re-enables and the bubble shows "Task cancelled." within ~1.5s. Click
   * Stop twice quickly — second call 409s silently, no toast.
   */
  async function handleStop() {
    const id = $activeTaskId;
    if (!id || cancelling) return;
    cancelling = true;
    try {
      await cancelTask(id);
    } catch (e) {
      console.error('[InputArea] cancelTask failed:', e);
    } finally {
      cancelling = false;
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      handleSubmit();
    }
  }

  // When the active project changes, refocus the textarea so the user can
  // start typing immediately.
  $: if ($activeProjectId && textareaEl) {
    textareaEl.focus();
  }
</script>

<footer class="border-t border-foundry-light-border dark:border-foundry-border bg-foundry-light-bg dark:bg-foundry-bg px-4 py-3">
  <div class="mx-auto flex max-w-3xl flex-col gap-2">
    <div class="flex items-end gap-2">
      <textarea
        bind:this={textareaEl}
        bind:value={input}
        placeholder={$isRunning ? 'Task running — type to retask, or press Stop…' : `Message ${$activeProject.name}…`}
        rows="2"
        class="flex-1 resize-none rounded-lg border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-surface dark:bg-foundry-surface text-foundry-light-text dark:text-foundry-text px-3 py-2 text-sm shadow-sm focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none placeholder:text-foundry-light-muted dark:placeholder:text-foundry-text/40"
        on:keydown={handleKeydown}
      ></textarea>
      {#if $isRunning}
        <!-- #3063: rectangular danger-styled Stop control — no `foundry-danger`
             token exists in tailwind.config.js yet, so this uses plain red
             utilities consistent with the red states already used elsewhere
             in this app (e.g. Sidebar's error dot, TaskHistory's failed
             badge) rather than inventing a new design-system color. -->
        <button
          type="button"
          class="inline-flex items-center gap-1 rounded-lg border border-red-700 bg-red-600 px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-red-700 disabled:cursor-not-allowed disabled:opacity-60"
          on:click={handleStop}
          disabled={cancelling}
        >
          <Square class="h-4 w-4" fill="currentColor" />
          {cancelling ? 'Stopping…' : 'Stop'}
        </button>
      {/if}
      <button
        type="button"
        class="inline-flex items-center gap-1 rounded-lg bg-foundry-light-primary dark:bg-foundry-primary px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:bg-foundry-light-surface dark:disabled:bg-foundry-surface disabled:text-foundry-light-muted dark:disabled:text-foundry-text/40"
        on:click={handleSubmit}
        {disabled}
      >
        <Send class="h-4 w-4" />
        Send
      </button>
    </div>

    <div class="flex items-center text-xs text-foundry-light-muted dark:text-foundry-text/70">
      <span class="ml-auto text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        {$isRunning ? 'Enter to stop + retask with this message, Shift+Enter for newline' : 'Enter to send, Shift+Enter for newline'}
      </span>
    </div>
  </div>
</footer>
