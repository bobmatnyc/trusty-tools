<script lang="ts">
  import { onMount } from 'svelte';
  import { Folder, Terminal, Loader2, Plus } from 'lucide-svelte';
  import { apiBase } from '../lib/api-config';
  import { projects, activeProjectId, isRunning, getCurrentApiToken } from '../stores/app';
  import TaskHistory from './TaskHistory.svelte';
  import LogoMark from '../lib/icons/LogoMark.svelte';

  export let apiReady = false;
  export let apiError = '';

  let clearing = false;

  function selectProject(id: string) {
    activeProjectId.set(id);
  }

  /**
   * Why: `POST /api/clear-context` now aborts any in-flight task as of
   * #3196, whereas it used to just wipe the in-memory task store. Both
   * "Clear Context" (footer) and "+ NEW TASK" (#3222, above) route through
   * this same call, so warning the user before silently killing a running
   * task belongs here once rather than duplicated per caller.
   * What: Returns true immediately when nothing is running; otherwise shows
   * a native `confirm()` and returns the user's choice.
   * Test: Manual — start a task, click either button, confirm the dialog
   * appears and Cancel leaves the task running.
   */
  function confirmIfRunning(): boolean {
    if (!$isRunning) return true;
    return confirm('A task is currently running. This will stop it and clear the chat. Continue?');
  }

  /**
   * Why: Lets users wipe accumulated task history and in-flight sessions
   * without restarting the server — a common need during iterative
   * development, and (#3222) the target of the "+ NEW TASK" button.
   * What: POSTs to `/api/clear-context` then reloads the page so the UI
   * reflects the empty task store. Uses `apiBase()` (not a bare relative
   * path) so this also works under Tauri, where the webview's own origin is
   * NOT the `trusty-agents --api` sidecar's `127.0.0.1:8765` — a relative
   * fetch would silently 404/fail to reach the server there.
   * Test: Click button, confirm network request returns {cleared:true}, confirm
   * page reloads and task list is empty.
   */
  async function handleClearContext() {
    if (!confirmIfRunning()) return;
    clearing = true;
    try {
      const token = getCurrentApiToken();
      const headers: Record<string, string> = token ? { Authorization: `Bearer ${token}` } : {};
      await fetch(`${apiBase()}/api/clear-context`, { method: 'POST', headers });
    } finally {
      window.location.reload();
    }
  }

  onMount(() => {
    // Nothing to hydrate yet — task history refresh lives in TaskHistory.
  });
</script>

<aside class="flex h-full w-72 flex-col border-r border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface">
  <header class="flex flex-col gap-1 border-b border-foundry-light-border dark:border-foundry-border px-4 py-3">
    <div class="flex items-center gap-2">
      <LogoMark size={20} />
    </div>
    <div class="flex items-center gap-1 text-xs">
      {#if apiReady}
        <span class="inline-block h-2 w-2 rounded-full bg-foundry-teal"></span>
        <span class="text-foundry-light-muted dark:text-foundry-text/70">API ready</span>
      {:else if apiError}
        <span class="inline-block h-2 w-2 rounded-full bg-red-500"></span>
        <span class="truncate text-red-500 dark:text-red-400" title={apiError}>API error</span>
      {:else}
        <Loader2 class="h-3 w-3 animate-spin text-foundry-amber" />
        <span class="text-foundry-light-muted dark:text-foundry-text/60">Starting…</span>
      {/if}
    </div>
  </header>

  <!-- #3222: "+ NEW TASK" — Foundry mockup (docs/design/gui/Foundry
       Ecosystem.dc.html:167), full-width rectangular button above TASK
       HISTORY. Reuses the same clear-context flow as the footer's "Clear
       Context" button (both now confirm first when a task is running, since
       #3196 made clear-context abort in-flight work). -->
  <div class="px-3 pt-3 pb-1">
    <button
      type="button"
      class="flex w-full items-center justify-center gap-1.5 rounded-md border border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary dark:bg-foundry-primary px-3 py-2 text-xs font-semibold uppercase tracking-wide text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-50"
      disabled={clearing}
      on:click={handleClearContext}
    >
      <Plus class="h-3.5 w-3.5" />
      New Task
    </button>
  </div>

  <nav class="flex flex-col gap-1 px-2 py-3">
    <h2 class="mb-1 px-2 text-xs font-semibold uppercase tracking-wide text-foundry-teal">
      Projects
    </h2>
    {#each $projects as project (project.id)}
      <button
        type="button"
        class="flex items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition-colors {project.id === $activeProjectId
          ? 'bg-foundry-light-primary/20 dark:bg-foundry-primary/20 text-foundry-light-text dark:text-foundry-text border-l-2 border-foundry-light-primary dark:border-foundry-primary'
          : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
        on:click={() => selectProject(project.id)}
      >
        <span
          class="inline-block h-2 w-2 rounded-full {project.status === 'running'
            ? 'bg-foundry-amber animate-pulse'
            : project.status === 'error'
              ? 'bg-red-500'
              : 'bg-foundry-light-muted/40 dark:bg-foundry-text/30'}"
        ></span>
        {#if project.id === 'ctrl'}
          <Terminal class="h-4 w-4" />
        {:else}
          <Folder class="h-4 w-4" />
        {/if}
        <span class="flex-1 truncate">{project.name}</span>
      </button>
    {/each}

  </nav>

  <div class="flex-1 overflow-y-auto border-t border-foundry-light-border dark:border-foundry-border px-2 py-3">
    <TaskHistory />
  </div>

  <footer class="flex flex-col gap-2 border-t border-foundry-light-border dark:border-foundry-border px-2 py-2">
    <button
      type="button"
      class="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-xs text-foundry-light-muted dark:text-foundry-text/50 hover:bg-red-100 dark:hover:bg-red-900/20 hover:text-red-600 dark:hover:text-red-400 disabled:opacity-40"
      disabled={clearing}
      on:click={handleClearContext}
    >
      <span class="text-base leading-none">&#x1F5D1;</span>
      {clearing ? 'Clearing…' : 'Clear Context'}
    </button>
  </footer>
</aside>
