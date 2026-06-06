<script lang="ts">
  import { onMount } from 'svelte';
  import { Folder, Terminal, Loader2 } from 'lucide-svelte';
  import { projects, activeProjectId } from '../stores/app';
  import TaskHistory from './TaskHistory.svelte';
  import LogoMark from '../lib/icons/LogoMark.svelte';

  export let apiReady = false;
  export let apiError = '';

  let clearing = false;

  function selectProject(id: string) {
    activeProjectId.set(id);
  }

  /**
   * Why: Lets users wipe accumulated task history and in-flight sessions
   * without restarting the server — a common need during iterative development.
   * What: POSTs to /api/clear-context then reloads the page so the UI reflects
   * the empty task store.
   * Test: Click button, confirm network request returns {cleared:true}, confirm
   * page reloads and task list is empty.
   */
  async function handleClearContext() {
    clearing = true;
    try {
      await fetch('/api/clear-context', { method: 'POST' });
    } finally {
      window.location.reload();
    }
  }

  onMount(() => {
    // Nothing to hydrate yet — task history refresh lives in TaskHistory.
  });
</script>

<aside class="flex h-full w-72 flex-col border-r border-ompm-light-border dark:border-ompm-border bg-ompm-light-surface dark:bg-ompm-surface">
  <header class="flex flex-col gap-1 border-b border-ompm-light-border dark:border-ompm-border px-4 py-3">
    <div class="flex items-center gap-2">
      <LogoMark size={20} />
    </div>
    <div class="flex items-center gap-1 text-xs">
      {#if apiReady}
        <span class="inline-block h-2 w-2 rounded-full bg-ompm-teal"></span>
        <span class="text-ompm-light-muted dark:text-ompm-text/70">API ready</span>
      {:else if apiError}
        <span class="inline-block h-2 w-2 rounded-full bg-red-500"></span>
        <span class="truncate text-red-500 dark:text-red-400" title={apiError}>API error</span>
      {:else}
        <Loader2 class="h-3 w-3 animate-spin text-ompm-amber" />
        <span class="text-ompm-light-muted dark:text-ompm-text/60">Starting…</span>
      {/if}
    </div>
  </header>

  <nav class="flex flex-col gap-1 px-2 py-3">
    <h2 class="mb-1 px-2 text-xs font-semibold uppercase tracking-wide text-ompm-teal">
      Projects
    </h2>
    {#each $projects as project (project.id)}
      <button
        type="button"
        class="flex items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition-colors {project.id === $activeProjectId
          ? 'bg-ompm-primary/20 text-ompm-light-text dark:text-ompm-text border-l-2 border-ompm-primary'
          : 'text-ompm-light-text/80 dark:text-ompm-text/80 hover:bg-ompm-primary/10'}"
        on:click={() => selectProject(project.id)}
      >
        <span
          class="inline-block h-2 w-2 rounded-full {project.status === 'running'
            ? 'bg-ompm-amber animate-pulse'
            : project.status === 'error'
              ? 'bg-red-500'
              : 'bg-ompm-light-muted/40 dark:bg-ompm-text/30'}"
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

  <div class="flex-1 overflow-y-auto border-t border-ompm-light-border dark:border-ompm-border px-2 py-3">
    <TaskHistory />
  </div>

  <footer class="flex flex-col gap-2 border-t border-ompm-light-border dark:border-ompm-border px-2 py-2">
    <button
      type="button"
      class="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-xs text-ompm-light-muted dark:text-ompm-text/50 hover:bg-red-100 dark:hover:bg-red-900/20 hover:text-red-600 dark:hover:text-red-400 disabled:opacity-40"
      disabled={clearing}
      on:click={handleClearContext}
    >
      <span class="text-base leading-none">&#x1F5D1;</span>
      {clearing ? 'Clearing…' : 'Clear Context'}
    </button>
  </footer>
</aside>
