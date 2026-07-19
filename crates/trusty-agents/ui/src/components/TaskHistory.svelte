<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { taskHistory } from '../stores/app';
  import type { TaskHistoryEntry } from '../stores/app';
  import { invoke, listenEvent, type UnlistenFn } from '../lib/transport';
  import type { PmResponseLike } from '../stores/workflow';

  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let errorMessage = '';

  /**
   * Why (#3221): `GET /api/tasks` (wired via `invoke('list_tasks')`) returns
   * the server's full `PmResponse` array — it carries `phases_completed`
   * even though the frontend's `TaskHistoryEntry` interface (stores/app.ts)
   * only declares the narrower fields TaskHistory has historically rendered.
   * Declaring the extra field locally (rather than widening the shared
   * interface) keeps this a self-contained, additive read of data that was
   * already on the wire, with no risk to other TaskHistoryEntry consumers.
   * What: Same shape as `TaskHistoryEntry` plus the optional raw
   * `phases_completed` list, reusing `PmResponseLike`'s field (#3217's
   * `stores/workflow.ts`) instead of a second, drifting inline duplicate of
   * that shape — `phaseStamp` below only reads `.name`/`.status` off it.
   */
  type TaskHistoryEntryWithPhases = TaskHistoryEntry & Pick<PmResponseLike, 'phases_completed'>;

  /**
   * Why: The mockup's task-history stamps read as present-continuous verbs
   * ("IMPLEMENTING") rather than the bare phase noun ("IMPLEMENT"). A small
   * static map covers the known workflow phase vocabulary; anything outside
   * it degrades gracefully to "<NAME>…" rather than guessing an inflection.
   */
  const CONTINUOUS_LABEL: Record<string, string> = {
    research: 'RESEARCHING',
    plan: 'PLANNING',
    implement: 'IMPLEMENTING',
    verify: 'VERIFYING',
    qa: 'TESTING',
    docs: 'DOCUMENTING',
  };

  /**
   * Why: Extends the coarse running/completed/failed pill with the
   * current/last phase name when the entry carries `phases_completed`,
   * matching the mockup's "IMPLEMENTING"/"✓ COMPLETE"/"✕ FAILED" stamps.
   * What: Terminal statuses keep the existing ✓/✕ labels (phase data adds
   * no information once the task is done); a running task with phase data
   * shows the last phase's continuous-form label instead of the generic
   * "running" word. Returns `null` when there's no phase data so the
   * caller falls back to the existing coarse pill unchanged.
   * Test: Manual — see PR description; covered visually via `pnpm dev`
   * against a running server with an in-flight prescriptive-workflow task.
   */
  function phaseStamp(entry: TaskHistoryEntry): string | null {
    const phases = (entry as TaskHistoryEntryWithPhases).phases_completed;
    if (!phases || phases.length === 0) return null;
    if (entry.status === 'completed' || entry.status === 'success') return '✓ COMPLETE';
    if (entry.status === 'failed' || entry.status === 'error') return '✕ FAILED';
    if (entry.status !== 'running') return null;
    const last = phases[phases.length - 1];
    return CONTINUOUS_LABEL[last.name] ?? `${last.name.toUpperCase()}…`;
  }

  /**
   * Why: Gives the user an at-a-glance view of recent workflow runs so they
   * can see whether prior bake-off tasks succeeded and what they cost.
   * What: Fetches list_tasks on demand; caps the display at 10 entries.
   */
  async function refresh() {
    try {
      const raw = await invoke<unknown>('list_tasks');
      const arr = Array.isArray(raw) ? (raw as TaskHistoryEntryWithPhases[]) : [];
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
  <h2 class="mb-2 px-2 text-xs font-semibold uppercase tracking-wide text-foundry-teal">
    Recent tasks
  </h2>

  {#if errorMessage}
    <p class="px-2 text-xs text-red-500 dark:text-red-400">{errorMessage}</p>
  {:else if $taskHistory.length === 0}
    <p class="px-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">No tasks yet.</p>
  {/if}

  <ul class="flex flex-col gap-1">
    {#each $taskHistory.filter(t => (t.task?.trim() || t.narrative?.trim()) || t.status !== 'running') as entry (entry.id)}
      <li class="flex flex-col rounded-md px-2 py-1 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10">
        <span class="truncate text-xs font-medium text-foundry-light-text dark:text-foundry-text" title={entry.task}>
          {shortTask(entry.task)}
        </span>
        <span class="flex items-center gap-2 text-[10px] text-foundry-light-muted dark:text-foundry-text/60">
          <span
            class="rounded px-1 py-0.5 font-medium {entry.status === 'running'
              ? 'bg-amber-500/20 text-amber-700 dark:text-amber-300'
              : entry.status === 'completed' || entry.status === 'success'
                ? 'bg-foundry-teal/20 text-foundry-teal'
                : entry.status === 'failed' || entry.status === 'error'
                  ? 'bg-red-500/20 text-red-600 dark:text-red-400'
                  : 'bg-foundry-light-border dark:bg-foundry-surface text-foundry-light-muted dark:text-foundry-text/70'}"
          >{phaseStamp(entry) ?? entry.status}</span>
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
