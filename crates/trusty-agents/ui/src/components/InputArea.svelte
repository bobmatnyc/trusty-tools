<script lang="ts">
  import { Send, Square } from 'lucide-svelte';
  import {
    activeAgentId,
    activeMessages,
    activeModelEntry,
    activeProject,
    activeProjectId,
    activeTaskId,
    addMessage,
    isRunning,
    replaceMessageTaskId,
    setProjectStatus,
    updateMessageByTask,
    type Project,
  } from '../stores/app';
  import { get } from 'svelte/store';
  import { cancelTask, invoke, listenEvent, type CancelTaskResult } from '../lib/transport';
  import { buildRetaskPayload, isPendingTaskId } from '../lib/retask';
  import { resolveOverride } from '../lib/models';

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
   * Why: `handleStop` needs a way to queue a cancel against a submission
   * that's still on its client-side `pending-<ts>` placeholder id (code-
   * critic finding on #3259: firing `cancelTask` against the placeholder
   * 404s and silently no-ops while the task keeps running). Rather than
   * disabling the Stop button until reconciliation — which would make it
   * look broken for the ~1.5s window most tasks spend there — we let the
   * click register immediately and fire the real cancel the moment the
   * submission's `task-progress` listener reconciles the placeholder to a
   * real id.
   * What: Set to the in-flight submission's `queueCancel` callback (and
   * cleared back to `null`) by `submitTask`; `null` when no submission is
   * active. `handleStop` calls it instead of `cancelTask` directly whenever
   * `activeTaskId` is still a placeholder.
   */
  let queueCancelForCurrentSubmission: (() => void) | null = null;

  /**
   * Why: Consumes `CancelTaskResult.http_status` (previously an unwired
   * field — code-critic LOW finding on #3259) to log a status-appropriate
   * diagnostic rather than just discarding the outcome. Not user-facing —
   * per #3063, 404/409 are expected races and must NOT toast — but a debug
   * trail is cheap and helps when triaging a "Stop didn't seem to work"
   * report.
   * What: Logs at `debug` for the 200/404/409 outcomes the backend
   * contract documents, `warn` for anything else.
   * Test: Manual — observe console output for each of the three documented
   * outcomes.
   */
  function logCancelOutcome(result: CancelTaskResult): void {
    switch (result.http_status) {
      case 200:
        console.debug('[InputArea] cancelTask: cancelled', result.id);
        break;
      case 404:
        console.debug('[InputArea] cancelTask: unknown task id (already gone)', result.id);
        break;
      case 409:
        console.debug('[InputArea] cancelTask: task already reached a terminal state', result);
        break;
      default:
        console.warn('[InputArea] cancelTask: unexpected http_status', result);
    }
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

    // Why (code-critic finding on #3259): a Stop/retask click that lands
    // before the real task id is known would otherwise fire `cancelTask`
    // against the `pending-<ts>` placeholder, which the backend has never
    // heard of — a silent 404 no-op while the task keeps running. `queueCancel`
    // lets `handleStop`/`handleSubmit` register "cancel as soon as you know
    // the real id" instead. Assigned synchronously (no `await` between here
    // and the isRunning/activeTaskId writes above) so there's no window where
    // a click could see a stale `null`.
    let cancelQueued = false;
    const queueCancel = () => {
      cancelQueued = true;
    };
    queueCancelForCurrentSubmission = queueCancel;

    // Why: `send_message` only resolves with the real task id at the END of
    // the run. In the meantime, `task-progress` events fire with the real
    // backend id — but the placeholder bubble is tagged with `pending-<ts>`,
    // so `updateMessageByTask` would never match and progress would silently
    // drop. We attach a one-shot listener that catches the first progress
    // event for THIS submission and swaps the placeholder id for the real
    // one, after which subsequent progress events route correctly. It's also
    // the reconciliation point for a queued cancel (see `queueCancel` above).
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
        if (cancelQueued) {
          cancelQueued = false;
          cancelTask(p.task_id)
            .then((result) => logCancelOutcome(result))
            .catch((e) => console.error('[InputArea] queued cancelTask failed:', e))
            .finally(() => {
              cancelling = false;
            });
        }
        unlistenReconcile?.();
      },
    );
    unlistenReconcile = unlistenP;

    try {
      // #3223 (regression fix, code-critic on PR #3279): only forward
      // `agent` when the user has explicitly picked a roster entry —
      // `activeAgentId` defaults to `null` (see `stores/app.ts` doc
      // comment) specifically so a default message OMITS this field and
      // falls through to the tools-armed `run_pm_task_with_session` path in
      // `handlers.rs`, rather than being forced onto the tools-off persona
      // path by sending a non-empty `agent` value on every submission.
      const invokeArgs: Record<string, unknown> = {
        content: payloadTask,
        projectPath: project.path ?? null,
      };
      const selectedAgent = get(activeAgentId);
      if (selectedAgent) {
        invokeArgs.agent = selectedAgent;
      }
      // #3245: forward the model/provider picker's selection, when any.
      // `resolveOverride` returns `{ modelId: null, providerId: null }` for
      // both the "Default" row and a `null` store (nothing selected yet),
      // so a session that never touches the picker omits both keys here —
      // identical wire shape to every pre-#3245 submission.
      const selectedModel = get(activeModelEntry);
      if (selectedModel) {
        const { modelId, providerId } = resolveOverride(selectedModel);
        if (modelId) invokeArgs.modelId = modelId;
        if (providerId) invokeArgs.providerId = providerId;
      }
      const result = await invoke<string>('send_message', invokeArgs);
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
      if (queueCancelForCurrentSubmission === queueCancel) {
        queueCancelForCurrentSubmission = null;
      }
      if (cancelQueued) {
        // The submission ended (error, or completed with no intervening
        // task-progress event) before the reconcile handler above ever ran
        // — there's no real id to cancel and nothing left to reconcile
        // against, so clear the "Stopping…" state here instead.
        cancelling = false;
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
      if (runningId && isPendingTaskId(runningId)) {
        // Same 404-no-op race as Stop (see `handleStop`/`queueCancel`): the
        // real backend id isn't known yet, so queue instead of firing
        // against the placeholder. Best-effort either way — we proceed with
        // the resubmit regardless of whether the queued cancel ultimately
        // lands.
        queueCancelForCurrentSubmission?.();
      } else if (runningId) {
        try {
          const result = await cancelTask(runningId);
          logCancelOutcome(result);
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
   * no-longer-wanted task without waiting for it to finish. `submitTask`'s
   * own `finally` block (see above) is what flips `isRunning`/`activeTaskId`
   * back to idle, once the aborted run's poll loop (Tauri: Rust; browser:
   * `fetchFallback`) observes the resulting `status: "cancelled"` on its
   * next tick — bounded by the 1.5s poll interval in both transports.
   * What: If `activeTaskId` is still the client-side `pending-<ts>`
   * placeholder (real backend id not yet reconciled — code-critic finding
   * on #3259), queues the cancel via `queueCancelForCurrentSubmission`
   * instead of firing it against an id the backend has never heard of
   * (which would 404 and silently no-op while the task keeps running); the
   * button stays disabled/"Stopping…" until the queued cancel actually
   * fires from `submitTask`'s reconcile listener. Otherwise cancels
   * immediately. 404 (already gone) and 409 (already terminal) are both
   * treated as success-adjacent per the backend contract — no error toast,
   * since the aborted run's own terminal-state handling in `submitTask`
   * reconciles the UI regardless. Only a genuine transport failure is
   * logged as an error; every outcome is logged via `logCancelOutcome`.
   * Test: Manual — start a long task, click Stop, observe the input
   * re-enables and the bubble shows "Task cancelled." within ~1.5s. Click
   * Stop twice quickly — second call 409s silently, no toast. Click Stop
   * immediately after Send (before the first `task-progress` event) —
   * observe the button shows "Stopping…" and the task is still cancelled
   * once reconciled, rather than silently doing nothing.
   */
  async function handleStop() {
    const id = $activeTaskId;
    if (!id || cancelling) return;
    cancelling = true;
    if (isPendingTaskId(id)) {
      queueCancelForCurrentSubmission?.();
      return;
    }
    try {
      const result = await cancelTask(id);
      logCancelOutcome(result);
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
