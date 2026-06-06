<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { taskHistory } from '../stores/app';
  import type { TaskHistoryEntry } from '../stores/app';
  import { invoke, listenEvent, type UnlistenFn } from '../lib/transport';

  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let errorMessage = '';

  /**
   * Why: Gives the user an at-a-glance view of recent workflow runs so they
   * can see whether prior bake-off tasks succeeded and what they cost.
   * What: Fetches list_tasks on demand; caps the display at 10 entries.
   */
  async function refresh() {
    try {
      const raw = await invoke<unknown>('list_tasks');
      const arr = Array.isArray(raw) ? (raw as TaskHistoryEntry[]) : [];
      taskHistory.set(arr.slice(0, 10));
      errorMessage = '';
    } catch (e) {
      errorMessage = `${e}`;
    }
  }

  /**
   * Why: Polling every 10s was redundant once task-complete/task-error events
   * were wired up — the in-process event bus delivers immediately when a task
   * finishes in the same browser context. No polling needed.
   * What: Refresh on mount (initial load) and on every task-complete/task-error
   * event. No interval.
   */
  onMount(async () => {
    refresh();
    unlistenComplete = await listenEvent('task-complete', () => refresh());
    unlistenError = await listenEvent('task-error', () => refresh());
  });

  onDestroy(() => {
    unlistenComplete?.();
    unlistenError?.();
  });

  function shortTask(t: string): string {
    if (!t) return '(empty)';
    return t.length > 40 ? t.slice(0, 40) + '…' : t;
  }

  function fmtCost(c?: number): string {
    if (typeof c !== 'number') return '';
    if (c < 0.01) return `$${c.toFixed(4)}`;
    return `$${c.toFixed(2)}`;
  }
</script>

<section>
  <h2 class="mb-2 px-2 text-xs font-semibold uppercase tracking-wide text-ompm-teal">
    Recent tasks
  </h2>

  {#if errorMessage}
    <p class="px-2 text-xs text-red-500 dark:text-red-400">{errorMessage}</p>
  {:else if $taskHistory.length === 0}
    <p class="px-2 text-xs text-ompm-light-muted dark:text-ompm-text/40">No tasks yet.</p>
  {/if}

  <ul class="flex flex-col gap-1">
    {#each $taskHistory.filter(t => (t.task?.trim() || t.narrative?.trim()) || t.status !== 'running') as entry (entry.id)}
      <li class="flex flex-col rounded-md px-2 py-1 hover:bg-ompm-primary/10">
        <span class="truncate text-xs font-medium text-ompm-light-text dark:text-ompm-text" title={entry.task}>
          {shortTask(entry.task)}
        </span>
        <span class="flex items-center gap-2 text-[10px] text-ompm-light-muted dark:text-ompm-text/60">
          <span
            class="rounded px-1 py-0.5 font-medium {entry.status === 'running'
              ? 'bg-amber-500/20 text-amber-700 dark:text-amber-300'
              : entry.status === 'completed' || entry.status === 'success'
                ? 'bg-ompm-teal/20 text-ompm-teal'
                : entry.status === 'failed' || entry.status === 'error'
                  ? 'bg-red-500/20 text-red-600 dark:text-red-400'
                  : 'bg-ompm-light-border dark:bg-ompm-surface text-ompm-light-muted dark:text-ompm-text/70'}"
          >{entry.status}</span>
          {#if typeof entry.score === 'number' && typeof entry.score_max === 'number'}
            <span class="font-mono">{entry.score}/{entry.score_max}</span>
          {/if}
          {#if entry.cost_usd}
            <span class="font-mono">{fmtCost(entry.cost_usd)}</span>
          {/if}
        </span>
      </li>
    {/each}
  </ul>
</section>
