<script lang="ts">
  import { Send } from 'lucide-svelte';
  import {
    activeProject,
    activeProjectId,
    addMessage,
    isRunning,
    replaceMessageTaskId,
    setProjectStatus,
    updateMessageByTask,
  } from '../stores/app';
  import { invoke, listenEvent } from '../lib/transport';

  let input = '';
  let textareaEl: HTMLTextAreaElement;

  $: disabled = $isRunning || !input.trim();

  /**
   * Why: Submits the typed message to the active project/CTRL context. We
   * create the user bubble immediately, then an empty assistant placeholder
   * tagged with a client-generated placeholder id; the Tauri `send_message`
   * command returns the real task id which replaces the placeholder so
   * `task-progress` events stream into the correct bubble.
   * What: Creates messages, calls `invoke('send_message')`, handles success
   * and failure paths.
   * Test: Type "hello", press Enter — a user bubble appears, then an
   * assistant bubble filling with progress, then the final narrative.
   */
  async function handleSubmit() {
    const content = input.trim();
    if (!content || $isRunning) return;

    const project = $activeProject;
    const projectId = project.id;
    const now = Date.now();

    addMessage(projectId, {
      id: `user-${now}`,
      role: 'user',
      content,
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

    input = '';
    isRunning.set(true);
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
        if (reconciled || !p.task_id) return;
        reconciled = true;
        replaceMessageTaskId(projectId, placeholderTaskId, p.task_id);
        // Apply the message that triggered the swap so it isn't lost.
        updateMessageByTask(projectId, p.task_id, p.message);
        unlistenReconcile?.();
      },
    );
    unlistenReconcile = unlistenP;

    try {
      const result = await invoke<string>('send_message', {
        content,
        projectPath: project.path ?? null,
      });
      // When `send_message` resolves (Tauri mode), the complete event should
      // already have updated the bubble. If not (browser fallback), we apply
      // the returned narrative directly.
      if (typeof result === 'string' && result.length > 0) {
        updateMessageByTask(projectId, placeholderTaskId, result);
      }
      setProjectStatus(projectId, 'idle');
    } catch (e) {
      updateMessageByTask(projectId, placeholderTaskId, `Error: ${e}`);
      setProjectStatus(projectId, 'error');
    } finally {
      // Detach reconcile listener if it never fired (e.g. error before any
      // progress event); leaking listeners across submissions would compound.
      unlistenReconcile?.();
      isRunning.set(false);
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

<footer class="border-t border-ompm-light-border dark:border-ompm-border bg-ompm-light-bg dark:bg-ompm-bg px-4 py-3">
  <div class="mx-auto flex max-w-3xl flex-col gap-2">
    <div class="flex items-end gap-2">
      <textarea
        bind:this={textareaEl}
        bind:value={input}
        placeholder={`Message ${$activeProject.name}…`}
        rows="2"
        class="flex-1 resize-none rounded-lg border border-ompm-light-border dark:border-ompm-primary/30 bg-ompm-light-surface dark:bg-ompm-surface text-ompm-light-text dark:text-ompm-text px-3 py-2 text-sm shadow-sm focus:border-ompm-primary focus:outline-none placeholder:text-ompm-light-muted dark:placeholder:text-ompm-text/40"
        on:keydown={handleKeydown}
        disabled={$isRunning}
      ></textarea>
      <button
        type="button"
        class="inline-flex items-center gap-1 rounded-lg bg-ompm-primary px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-ompm-primary/80 disabled:cursor-not-allowed disabled:bg-ompm-light-surface dark:disabled:bg-ompm-surface disabled:text-ompm-light-muted dark:disabled:text-ompm-text/40"
        on:click={handleSubmit}
        {disabled}
      >
        <Send class="h-4 w-4" />
        Send
      </button>
    </div>

    <div class="flex items-center text-xs text-ompm-light-muted dark:text-ompm-text/70">
      <span class="ml-auto text-[11px] text-ompm-light-muted dark:text-ompm-text/40">Enter to send, Shift+Enter for newline</span>
    </div>
  </div>
</footer>
