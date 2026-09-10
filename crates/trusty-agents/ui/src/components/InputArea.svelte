<script lang="ts">
  import ChatAttachments from './ChatAttachments.svelte';
  import { clipboardInputs } from '../lib/clipboardAttachments';
  import { composerDrafts, emptyDraft, setDraftText, setDraftError, addClipboardInputs, retryClipboardInputs, discardPendingPaste, removeDraftAttachment, clearSubmittedDraft, preserveFailedDraft, restoreFailedDraft } from '../stores/composerDrafts';
  import type { DraftAttachment } from '../lib/chatAttachments';
  import { ArrowUp, Paperclip, Square, X } from 'lucide-svelte';
  import {
    activeAgentId,
    conversationKey,
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
    type Message,
    type Project,
  } from '../stores/app';
  import { get } from 'svelte/store';
  import { cancelTask, invoke, listenEvent, type CancelTaskResult } from '../lib/transport';
  import { buildRetaskPayload, isPendingTaskId, type HistoryTurn } from '../lib/retask';
  import { resolveOverride, type PickerEntry } from '../lib/models';
  import { agentRoster } from '../stores/app';
  import { rosterDisplayName } from '../lib/roster';

  // #7370: files attached to this turn. They are uploaded BEFORE the send, so
  // the turn carries ids the server has already validated rather than bytes it
  // has to accept mid-dispatch.
  import { formatSize, uploadAttachments, type AttachmentRef } from '../lib/attachments';

  import ModelSwitcher from './ModelSwitcher.svelte';
  import { chatProjectPath, chatFolderError, detachUnavailableChatFolders } from '../stores/workspace';
  import { openConfigPane } from '../stores/configPane';

  type SubmissionContext = { draftKey: string; draftItems: DraftAttachment[]; projectPath: string | null; agent: string | null; model: PickerEntry | null; speaker: string; attachments: AttachmentRef[] };

  let input = '';
  $: draftKey = conversationKey($activeProjectId, $activeAgentId);
  $: draft = $composerDrafts[draftKey] ?? emptyDraft;
  $: input = draft.text;
  function paste(event: ClipboardEvent) {
    if (!event.clipboardData) return;
    try {
      const inputs = clipboardInputs(event.clipboardData);
      if (!inputs) return;
      event.preventDefault();
      void addClipboardInputs(draftKey, inputs);
    } catch (error) { event.preventDefault(); setDraftError(draftKey, String(error)); }
  }
  let textareaEl: HTMLTextAreaElement;
  let cancelling = false;

  /**
   * Why (#7370): an attachment is uploaded the moment it is picked, not at
   * send time — so the server's guards (traversal, size cap, media type) answer
   * while the user is still composing and can do something about it, rather
   * than failing a turn they have already committed to.
   * What: the rows the server returned for files staged on this draft. Cleared
   * when the turn is sent, so the next turn starts empty.
   */
  let pendingAttachments: AttachmentRef[] = [];
  let attachmentError: string | null = null;
  let uploading = false;
  let fileInput: HTMLInputElement;
  let dragging = false;

  $: disabled =
    (!input.trim() && pendingAttachments.length === 0 && !draft.items.length) ||
    draft.busy > 0 ||
    !!draft.pendingPaste ||
    !!$chatFolderError ||
    uploading;

  /**
   * Why: the upload is what turns a local `File` into something a turn can
   * reference. Errors are shown next to the composer rather than thrown,
   * because the user is mid-draft and the draft must survive.
   * What: one request for the whole selection, because dropping three files is
   * one gesture. The server refuses the batch whole on a bad name or an
   * oversize file, so a refusal stages nothing and the message it gives is the
   * one shown.
   * Test: `lib/attachments.test.ts` covers `uploadAttachments`' two arms.
   */
  async function stageFiles(files: File[]): Promise<void> {
    if (files.length === 0) return;
    uploading = true;
    attachmentError = null;
    try {
      const staged = await uploadAttachments(get(activeAgentId), files);
      pendingAttachments = [...pendingAttachments, ...staged];
    } catch (e) {
      attachmentError = `${e instanceof Error ? e.message : e}`;
    } finally {
      uploading = false;
    }
  }

  function onPick(event: Event): void {
    const picked = (event.target as HTMLInputElement).files;
    void stageFiles(picked ? Array.from(picked) : []);
    // Reset so picking the same file twice in a row still fires `change`.
    if (fileInput) fileInput.value = '';
  }

  /**
   * Why: dropping a FILE onto the composer stages an attachment. This is a
   * different gesture from the project-folder drop that sets `chatProjectPath`
   * (`stores/workspace`), which arrives as a Tauri native drop event carrying
   * paths, not as a DOM `DataTransfer` carrying `File` objects — so the two
   * cannot be confused. A drop with no `File` in it is ignored here and left
   * to whatever else is listening.
   */
  function onDrop(event: DragEvent): void {
    dragging = false;
    const dropped = Array.from(event.dataTransfer?.files ?? []);
    if (dropped.length === 0) return;
    event.preventDefault();
    void stageFiles(dropped);
  }

  function removeAttachment(id: string): void {
    pendingAttachments = pendingAttachments.filter((a) => a.id !== id);
  }

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
    context: SubmissionContext,
  ): Promise<void> {
    const mySeq = ++submissionSeq;
    const projectId = conversationKey(project.id, context.agent);
    const now = Date.now();

    addMessage(projectId, {
      id: `user-${now}`,
      role: 'user',
      content: displayContent,
      inlineAttachments: context.draftItems.map(item => item.attachment),
      timestamp: now,
      // #7370: the bubble shows the cards immediately. The server appends the
      // rendered attachment blocks to the PERSISTED turn, so a later reload
      // rebuilds these same cards from the markers in that content.
      ...(context.attachments.length ? { attachments: context.attachments } : {}),
    });

    // Assistant placeholder. `taskId` is set to a temp id and patched once
    // the backend returns the real id via the resolved promise OR the first
    // progress event (whichever arrives first).
    //
    // #3737: stamp the placeholder with the display name of the persona
    // active RIGHT NOW (the same `activeAgentId` this submission forwards as
    // its `agent` field below). Resolving it here — at message creation —
    // means a persona switch after this message is sent relabels only later
    // bubbles; this one keeps whoever produced it.
    const placeholderTaskId = `pending-${now}`;
    const speaker = context.speaker;
    addMessage(projectId, {
      id: `asst-${now}`,
      role: 'assistant',
      content: '',
      timestamp: now,
      taskId: placeholderTaskId,
      speaker,
    });

    isRunning.set(true);
    activeTaskId.set(placeholderTaskId);
    setProjectStatus(project.id, 'running');

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
    const unlistenP = await listenEvent<{ task_id: string }>(
      'task-progress',
      (p) => {
        if (reconciled || !p.task_id || mySeq !== submissionSeq) return;
        reconciled = true;
        // Reconcile the placeholder id to the real backend id — load-bearing:
        // it's what lets subsequent `task-delta`/`task-complete` events (keyed
        // by the real id) find and fill this bubble. We deliberately do NOT
        // write the progress message's status text into the bubble: interim
        // "Running…"/"submitted…" strings are not shown in the response bubble
        // (the spinner is the sole waiting indicator); the bubble stays empty
        // until the first streamed delta or the final narrative arrives.
        replaceMessageTaskId(projectId, placeholderTaskId, p.task_id);
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
        ...(context.draftItems.length ? { inlineAttachments: context.draftItems.map(item => item.attachment) } : {}),
        projectPath: context.projectPath,
      };
      const selectedAgent = context.agent;
      if (selectedAgent) {
        invokeArgs.agent = selectedAgent;
      }
      // #7370: ids only. The bytes are already on the server, validated; the
      // turn is refused outright if any id is not in the session's manifest.
      if (context.attachments.length) {
        invokeArgs.attachments = context.attachments.map((a) => a.id);
      }
      // #3245: forward the model/provider picker's selection, when any.
      // `resolveOverride` returns `{ modelId: null, providerId: null }` for
      // both the "Default" row and a `null` store (nothing selected yet),
      // so a session that never touches the picker omits both keys here —
      // identical wire shape to every pre-#3245 submission.
      const selectedModel = context.model;
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
      if (typeof result === 'string' && result.trim().length > 0) {
        updateMessageByTask(projectId, placeholderTaskId, result);
      }
      setProjectStatus(project.id, 'idle');
    } catch (e) {
      if (mySeq !== submissionSeq) return;
      updateMessageByTask(projectId, placeholderTaskId, `Error: ${e}`);
      setProjectStatus(project.id, 'error');
      preserveFailedDraft(context.draftKey, displayContent, context.draftItems);
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
    // #7370: an attachment alone is a valid turn — "here, look at this".
    if (
      (!content && pendingAttachments.length === 0 && !draft.items.length) ||
      draft.busy ||
      draft.pendingPaste ||
      get(chatFolderError)
    )
      return;

    const project = $activeProject;
    // Freeze all dispatch choices before listener setup or cancellation can yield.
    const context: SubmissionContext = {
      draftKey, draftItems: structuredClone(draft.items),
      projectPath: get(chatProjectPath) ?? project.path ?? null,
      agent: get(activeAgentId), model: get(activeModelEntry),
      speaker: rosterDisplayName(get(agentRoster), get(activeAgentId)),
      attachments: pendingAttachments,
    };
    const historyForRetask: HistoryTurn[] = $activeMessages
      .filter((m): m is Message & { role: HistoryTurn['role'] } => m.role !== 'topic-boundary')
      .map((m) => ({ role: m.role, content: m.content }));

    if ($isRunning) {
      const proceed = confirm(
        'A task is still running. Stop it and send this message instead?',
      );
      if (!proceed) return;

      const runningId = $activeTaskId;
      clearSubmittedDraft(context.draftKey);
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
      // #3819: `topic-boundary` rows are UI-only dividers ("+ New Task"),
      // not real conversation turns — excluded from the history payload
      // sent to the backend, same as they'd never have been included
      // before this role existed. Mapped (not just filtered) so the result
      // is a real `HistoryTurn[]`, not a `Message[]` the type checker still
      // sees as possibly carrying `'topic-boundary'`.
      const payload = buildRetaskPayload(historyForRetask, content);
      pendingAttachments = [];
      await submitTask(project, content, payload, context);
      return;
    }

    input = '';
    // Cleared before the await so a second Enter cannot send the same files
    // twice; `context` already holds them.
    pendingAttachments = [];
    attachmentError = null;
    clearSubmittedDraft(context.draftKey);
    await submitTask(project, content, content, context);
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

<!-- svelte-ignore a11y-no-static-element-interactions -->
<footer
  class="shrink-0 min-w-0 border-t border-foundry-light-border dark:border-foundry-border bg-foundry-light-bg dark:bg-foundry-bg p-3 {dragging ? 'ring-2 ring-inset ring-foundry-light-primary dark:ring-foundry-primary' : ''}"
  on:dragover|preventDefault={() => (dragging = true)}
  on:dragleave={() => (dragging = false)}
  on:drop={onDrop}
>
  {#if $chatFolderError}<p role="alert" class="mb-2 text-xs text-foundry-light-muted dark:text-foundry-text/70">{$chatFolderError} <button type="button" class="underline" on:click={() => { if (!detachUnavailableChatFolders()) openConfigPane(); }}>Resolve folder</button></p>{/if}
  <div class="flex w-full min-w-0 flex-col rounded-xl border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface shadow-sm focus-within:border-foundry-light-primary dark:focus-within:border-foundry-primary">
    <textarea
      bind:this={textareaEl}
      bind:value={input}
      aria-label="Message"
      placeholder={$isRunning ? 'Task running — type to retask, or press Stop…' : `Message ${rosterDisplayName($agentRoster, $activeAgentId)}…`}
      rows="2"
      class="w-full resize-none rounded-t-xl bg-transparent px-3 pt-3 pb-2 text-sm text-foundry-light-text dark:text-foundry-text focus:outline-none placeholder:text-foundry-light-muted dark:placeholder:text-foundry-text/40"
      on:keydown={handleKeydown}
      on:paste={paste}
      on:input={event => setDraftText(draftKey, event.currentTarget.value)}
    ></textarea>
    {#if attachmentError}
      <p role="alert" class="px-3 pb-1 text-xs text-red-600 dark:text-red-400">{attachmentError}</p>
    {/if}
    {#if pendingAttachments.length > 0}
      <ul class="flex flex-wrap gap-2 px-3 pb-2" data-pending-attachments>
        {#each pendingAttachments as attachment (attachment.id)}
          <li class="flex items-center gap-1 rounded-full border border-foundry-light-border dark:border-foundry-border px-2 py-0.5 text-xs text-foundry-light-text dark:text-foundry-text">
            <span class="max-w-[12rem] truncate">{attachment.file_name}</span>
            <span class="text-foundry-light-muted dark:text-foundry-text/50">{formatSize(attachment.size)}</span>
            <button
              type="button"
              aria-label={`Remove ${attachment.file_name}`}
              class="ml-0.5 rounded-full p-0.5 hover:bg-foundry-light-bg dark:hover:bg-foundry-bg"
              on:click={() => removeAttachment(attachment.id)}
            >
              <X class="h-3 w-3" />
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <ChatAttachments attachments={draft.items.map(item => item.attachment)} remove={index => removeDraftAttachment(draftKey, draft.items[index].id)} />
    {#if draft.busy}<p class="px-3 py-1 text-xs" role="status">Processing pasted files…</p>{/if}
    {#if draft.error}<p class="px-3 py-1 text-xs text-red-600 dark:text-red-400" role="alert">{draft.error}</p>{/if}
    {#if draft.pendingPaste}<div class="flex gap-3 px-3 py-1 text-xs"><button type="button" class="underline" disabled={draft.busy > 0} on:click={() => retryClipboardInputs(draftKey)}>Retry pasted files</button><button type="button" class="underline" disabled={draft.busy > 0} on:click={() => discardPendingPaste(draftKey)}>Discard failed paste</button></div>{/if}
    {#if draft.failed}<p class="px-3 py-1 text-xs">The failed message is available to retry. <button type="button" class="underline" on:click={() => restoreFailedDraft(draftKey)}>Restore failed message</button></p>{/if}
    <div class="flex min-w-0 items-center justify-between gap-2 px-2 pb-2">
      <div class="flex min-w-0 items-center gap-2">
        <input
          bind:this={fileInput}
          type="file"
          multiple
          class="hidden"
          aria-hidden="true"
          tabindex="-1"
          on:change={onPick}
        />
        <button
          type="button"
          aria-label="Attach a file"
          title={uploading ? 'Uploading…' : 'Attach a file'}
          class="inline-flex h-8 w-8 items-center justify-center rounded-full text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-bg dark:hover:bg-foundry-bg disabled:opacity-40"
          on:click={() => fileInput?.click()}
          disabled={uploading}
        >
          <Paperclip class="h-4 w-4" />
        </button>
        <ModelSwitcher />
      </div>
      <div class="flex shrink-0 items-center gap-2">
        {#if $isRunning}
          <button
            type="button"
            aria-label={cancelling ? 'Stopping task' : 'Stop task'}
            title={cancelling ? 'Stopping…' : 'Stop task'}
            class="inline-flex h-8 w-8 items-center justify-center rounded-full bg-red-600 text-white hover:bg-red-700 disabled:cursor-not-allowed disabled:opacity-60"
            on:click={handleStop}
            disabled={cancelling}
          >
            <Square class="h-3.5 w-3.5" fill="currentColor" />
          </button>
        {/if}
        <button
          type="button"
          aria-label="Send message"
          title={$isRunning ? 'Stop and send message' : 'Send message'}
          class="inline-flex h-8 w-8 items-center justify-center rounded-full bg-foundry-light-primary dark:bg-foundry-primary text-white hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-40"
          on:click={handleSubmit}
          {disabled}
        >
          <ArrowUp class="h-4 w-4" />
        </button>
      </div>
    </div>
  </div>
</footer>
