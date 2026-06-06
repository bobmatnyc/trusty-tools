<script lang="ts">
  /**
   * Why: A flat session-chip list buried project structure and made it hard
   * to spot which projects had live work happening. The new project-centric
   * accordion surfaces a Last-Connected-sorted list with quick badges (PRs,
   * issues, active sessions) and an expandable session drawer per project,
   * so operators can scan dozens of projects and drill into the one with
   * activity. (#370)
   * What: Renders sort controls + filter input, then a list of expandable
   * project cards. Each card shows name, framework, path/origin, last-active
   * timestamp, count badges (active sessions, PRs, issues), and on expansion
   * lists each session with a status pulse dot, adapter type, status pill,
   * and a connect-to-chat button. Auto-refreshes every 10s.
   * Test: Mount with mock projects from /api/projects, verify cards render
   * sorted by last_active desc, click a header to expand, click [→] to set
   * activeProjectId+activeView='chat'.
   */
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';
  import {
    RefreshCw,
    Github,
    Folder,
    ChevronRight,
    Bug,
    GitPullRequest,
    Plus,
    ArrowRight,
    Copy,
    X,
    Play,
    Pause,
    Trash2,
    Link2,
  } from 'lucide-svelte';
  import {
    projectsList,
    fetchProjects,
    activeProjectId,
    projects as projectStore,
    tmSessions,
    fetchTmSessions,
    tmApi,
    type Project,
  } from '../stores/app';
  import { relativeTime, githubUrl } from '../lib/utils';
  import { recaps } from '../stores/recap';

  const dispatch = createEventDispatcher<{ navigate: { view: 'chat' } }>();

  type SortMode = 'last_connected' | 'name' | 'active_first';

  let showAll = false;
  let loading = false;
  let error = '';
  let sortMode: SortMode = 'last_connected';
  let filter = '';
  let expanded = new Set<string>();
  let pollHandle: ReturnType<typeof setInterval> | null = null;
  // #450: TM session management state.
  let sessionError = '';
  let attachModalName: string | null = null;
  let attachCopied = false;

  // #458: Add-project-by-path form state.
  let showAddForm = false;
  let newPath = '';
  let addError = '';
  let addLoading = false;

  /**
   * Why: #458 — browser HTML5 validation rejects Unix paths in url-type inputs.
   * What: POSTs `{path}` to `/api/projects` using type="text" input so paths
   * like /Users/... and ~/... are accepted, then refreshes the project list.
   * Test: Enter /tmp/myproject, click Add — observe POST to /api/projects and
   * the new card appearing in the list.
   */
  async function submitAddProject() {
    const trimmed = newPath.trim();
    if (!trimmed) return;
    addError = '';
    addLoading = true;
    try {
      await tmApi('/api/projects', {
        method: 'POST',
        body: JSON.stringify({ path: trimmed }),
      });
      newPath = '';
      showAddForm = false;
      await fetchProjects(showAll);
    } catch (e) {
      addError = (e as Error).message;
    } finally {
      addLoading = false;
    }
  }

  async function refresh() {
    loading = true;
    error = '';
    try {
      await fetchProjects(showAll);
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  }

  function toggleShowAll() {
    showAll = !showAll;
    refresh();
  }

  function toggleExpand(id: string) {
    const next = new Set(expanded);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    expanded = next;
  }

  /**
   * Why: Connect-to-session needs to surface the project in the sidebar's
   * chat-target list (which only contains CTRL by default) and switch the
   * top-level view to chat. We dispatch a 'navigate' event so App.svelte
   * controls the activeView state — keeps view-routing in one place.
   * What: Ensures the project exists in the chat projects store, sets
   * activeProjectId, and emits navigate.
   * Test: Click [→] on a project, observe sidebar shows project as active
   * and the chat view replaces ProjectsView.
   */
  function connectToProject(project: Project) {
    projectStore.update((items) => {
      if (items.find((p) => p.id === project.id)) return items;
      return [
        ...items,
        {
          id: project.id,
          name: project.name,
          path: project.path,
          status: 'idle' as const,
        },
      ];
    });
    activeProjectId.set(project.id);
    dispatch('navigate', { view: 'chat' });
  }

  /**
   * Why: The "New Session" button previously stubbed out to a redirect,
   * which meant users couldn't actually create a tmux session from the
   * WebUI. Now we POST `/api/tm/sessions` with the project path and, on
   * success, surface the attach instructions in a modal so the user can
   * copy/paste `tmux attach-session -t <name>` into their terminal — the
   * WebUI can't exec tmux directly (#450).
   * What: Validates we have a path, calls `tmApi`, refreshes the live
   * session store, and opens the attach modal. Errors set `sessionError`
   * which the UI renders inline.
   * Test: Click "New Session" on a project, observe POST to /api/tm/sessions
   * and the attach modal opening with the returned session name.
   */
  async function newSession(project: Project) {
    if (!project.path) {
      sessionError = 'Cannot create session: project has no path';
      return;
    }
    sessionError = '';
    try {
      const data = await tmApi<{ name: string; status: string }>(
        '/api/tm/sessions',
        {
          method: 'POST',
          body: JSON.stringify({ project_path: project.path }),
        },
      );
      attachModalName = data.name;
      await fetchTmSessions();
    } catch (e) {
      sessionError = (e as Error).message;
    }
  }

  async function killSession(name: string) {
    sessionError = '';
    try {
      await tmApi(`/api/tm/sessions/${encodeURIComponent(name)}`, {
        method: 'DELETE',
      });
      await fetchTmSessions();
    } catch (e) {
      sessionError = (e as Error).message;
    }
  }

  async function pauseSession(name: string) {
    sessionError = '';
    try {
      await tmApi(`/api/tm/sessions/${encodeURIComponent(name)}/pause`, {
        method: 'POST',
      });
      await fetchTmSessions();
    } catch (e) {
      sessionError = (e as Error).message;
    }
  }

  async function resumeSession(name: string) {
    sessionError = '';
    try {
      await tmApi(`/api/tm/sessions/${encodeURIComponent(name)}/resume`, {
        method: 'POST',
      });
      await fetchTmSessions();
    } catch (e) {
      sessionError = (e as Error).message;
    }
  }

  function openAttachModal(name: string) {
    attachModalName = name;
  }

  function closeAttachModal() {
    attachModalName = null;
  }

  async function copyAttachCommand() {
    if (!attachModalName) return;
    const cmd = `tmux attach-session -t ${attachModalName}`;
    try {
      await navigator.clipboard.writeText(cmd);
      attachCopied = true;
      setTimeout(() => (attachCopied = false), 1500);
    } catch {
      // Clipboard may be blocked (insecure context); ignore silently.
    }
  }

  function statusDotClasses(status: string): string {
    switch (status) {
      case 'Running':
        return 'bg-green-500 session-pulse';
      case 'Idle':
      case 'Paused':
        return 'bg-ompm-light-muted/60 dark:bg-ompm-text/40';
      case 'Orphaned':
      case 'Stopped':
        return 'bg-red-500/60';
      default:
        return 'bg-ompm-light-muted/40 dark:bg-ompm-text/30';
    }
  }

  function statusPillClasses(status: string): string {
    switch (status) {
      case 'Running':
        return 'bg-green-500/15 text-green-700 dark:text-green-300';
      case 'Idle':
      case 'Paused':
        return 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-300';
      case 'Orphaned':
      case 'Stopped':
        return 'bg-red-500/15 text-red-700 dark:text-red-300';
      default:
        return 'bg-ompm-light-border/40 dark:bg-ompm-text/20 text-ompm-light-text/80 dark:text-ompm-text/70';
    }
  }

  function activeSessionCount(p: Project): number {
    return (p.sessions ?? []).filter((s) => s.status === 'Running').length;
  }

  function sortProjects(list: Project[], mode: SortMode): Project[] {
    return [...list].sort((a, b) => {
      if (mode === 'active_first') {
        const aActive = activeSessionCount(a) > 0 ? 0 : 1;
        const bActive = activeSessionCount(b) > 0 ? 0 : 1;
        if (aActive !== bActive) return aActive - bActive;
      }
      if (mode === 'name') return a.name.localeCompare(b.name);
      const aTime = a.last_active ? new Date(a.last_active).getTime() : 0;
      const bTime = b.last_active ? new Date(b.last_active).getTime() : 0;
      return bTime - aTime;
    });
  }

  onMount(() => {
    refresh();
    // Initial TM session fetch — best-effort, ignore 503 when tmux isn't
    // available so the project tree still renders.
    fetchTmSessions().catch(() => {});
    pollHandle = setInterval(() => {
      // Background refresh — don't toggle the loading flag so the UI doesn't
      // flicker; let errors surface through the visible error state.
      // Sessions poll via /api/tm/sessions (live tmux state, #450); project
      // tree polls /api/projects on the same cadence.
      fetchProjects(showAll).catch((e) => {
        error = (e as Error).message;
      });
      fetchTmSessions().catch(() => {
        // Quietly swallow — transient 503 (no tmux) shouldn't blank the UI.
      });
    }, 10_000);
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });

  /**
   * Why: Live tmux session data (status, adapter) lives in `tmSessions` and
   * updates every 10s, while project metadata (PRs, issues, framework) comes
   * from `/api/projects` which is computed less frequently. Overlaying the
   * live session list onto each project gives the UI a single source of
   * truth without duplicating polling. (#450)
   * What: For each project, replace `sessions` with the matching entries
   * from the live `tmSessions` store (matched by `project` path).
   * Test: Mount with a project at `/foo` and a tmSession with `project=/foo`,
   * assert the card's sessions array contains the live session.
   */
  $: rawCards = (($projectsList ?? []) as Project[]).map((p) => {
    const live = ($tmSessions ?? []).filter((s) => s.project === p.path);
    if (live.length === 0) return p;
    return {
      ...p,
      sessions: live.map((s) => ({
        name: s.name,
        adapter_type: s.adapter_type,
        status: s.status,
      })),
    };
  });
  $: filtered = filter.trim()
    ? rawCards.filter((p) => {
        const q = filter.toLowerCase();
        return (
          p.name.toLowerCase().includes(q) ||
          (p.path ?? '').toLowerCase().includes(q) ||
          (p.git_origin ?? '').toLowerCase().includes(q)
        );
      })
    : rawCards;
  $: cards = sortProjects(filtered, sortMode);
</script>

<section class="flex h-full flex-1 flex-col overflow-hidden bg-ompm-light-bg dark:bg-ompm-bg">
  <header
    class="flex flex-wrap items-center justify-between gap-3 border-b border-ompm-light-border dark:border-ompm-border px-6 py-3"
  >
    <div class="flex items-center gap-3">
      <h1 class="text-lg font-semibold text-ompm-light-text dark:text-ompm-text">Projects</h1>
      {#if cards.length > 0}
        <span class="text-xs text-ompm-light-muted dark:text-ompm-text/60">
          {cards.length} {cards.length === 1 ? 'project' : 'projects'}
        </span>
      {/if}
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <span class="text-xs text-ompm-light-muted dark:text-ompm-text/60">Sort by:</span>
      <div
        class="flex items-center gap-0.5 rounded-md border border-ompm-light-border dark:border-ompm-border p-0.5"
        role="group"
        aria-label="Sort mode"
      >
        <button
          type="button"
          class="rounded px-2 py-0.5 text-xs transition-colors {sortMode === 'last_connected'
            ? 'bg-ompm-primary text-white'
            : 'text-ompm-light-muted dark:text-ompm-text/70 hover:bg-ompm-primary/10'}"
          on:click={() => (sortMode = 'last_connected')}
        >
          Last Connected
        </button>
        <button
          type="button"
          class="rounded px-2 py-0.5 text-xs transition-colors {sortMode === 'name'
            ? 'bg-ompm-primary text-white'
            : 'text-ompm-light-muted dark:text-ompm-text/70 hover:bg-ompm-primary/10'}"
          on:click={() => (sortMode = 'name')}
        >
          Name
        </button>
        <button
          type="button"
          class="rounded px-2 py-0.5 text-xs transition-colors {sortMode === 'active_first'
            ? 'bg-ompm-primary text-white'
            : 'text-ompm-light-muted dark:text-ompm-text/70 hover:bg-ompm-primary/10'}"
          on:click={() => (sortMode = 'active_first')}
        >
          Active First
        </button>
      </div>
      <input
        type="search"
        placeholder="Filter…"
        bind:value={filter}
        class="rounded-md border border-ompm-light-border dark:border-ompm-border bg-ompm-light-bg dark:bg-ompm-bg px-2 py-1 text-xs text-ompm-light-text dark:text-ompm-text placeholder:text-ompm-light-muted dark:placeholder:text-ompm-text/40 focus:border-ompm-primary focus:outline-none"
      />
      <label class="flex items-center gap-1 text-xs text-ompm-light-muted dark:text-ompm-text/70">
        <input type="checkbox" checked={showAll} on:change={toggleShowAll} class="h-3 w-3" />
        Show all
      </label>
      <button
        type="button"
        on:click={refresh}
        disabled={loading}
        class="inline-flex items-center gap-1 rounded-md border border-ompm-light-border dark:border-ompm-border px-2 py-1 text-xs text-ompm-light-text dark:text-ompm-text hover:bg-ompm-primary/10 disabled:opacity-50"
        title="Refresh"
      >
        <RefreshCw class="h-3 w-3 {loading ? 'animate-spin' : ''}" />
        Refresh
      </button>
      <button
        type="button"
        on:click={() => { showAddForm = !showAddForm; addError = ''; }}
        class="inline-flex items-center gap-1 rounded-md border border-ompm-light-border dark:border-ompm-border px-2 py-1 text-xs text-ompm-light-text dark:text-ompm-text hover:bg-ompm-primary/10"
        title="Add project by path"
      >
        <Plus class="h-3 w-3" />
        Add
      </button>
    </div>
  </header>

  {#if showAddForm}
    <!--
      #458: type="text" is required — type="url" or a restrictive pattern= causes
      browser HTML5 validation to reject Unix filesystem paths (/Users/…, ~/…).
    -->
    <div
      class="flex items-center gap-2 border-b border-ompm-light-border dark:border-ompm-border bg-ompm-light-surface/60 dark:bg-ompm-surface/60 px-6 py-2"
    >
      <input
        type="text"
        bind:value={newPath}
        placeholder="/absolute/path/to/project  or  ~/relative/path"
        title="Enter an absolute or relative filesystem path"
        class="flex-1 rounded-md border border-ompm-light-border dark:border-ompm-border bg-ompm-light-bg dark:bg-ompm-bg px-2 py-1 text-xs text-ompm-light-text dark:text-ompm-text placeholder:text-ompm-light-muted dark:placeholder:text-ompm-text/40 focus:border-ompm-primary focus:outline-none"
        on:keydown={(e) => e.key === 'Enter' && submitAddProject()}
        disabled={addLoading}
      />
      <button
        type="button"
        on:click={submitAddProject}
        disabled={addLoading || !newPath.trim()}
        class="inline-flex items-center gap-1 rounded-md bg-ompm-primary px-3 py-1 text-xs font-medium text-white hover:bg-ompm-primary/80 disabled:opacity-50"
      >
        {addLoading ? 'Adding…' : 'Add'}
      </button>
      <button
        type="button"
        on:click={() => { showAddForm = false; newPath = ''; addError = ''; }}
        class="inline-flex items-center justify-center rounded-md border border-ompm-light-border dark:border-ompm-border p-1 text-ompm-light-muted dark:text-ompm-text/60 hover:bg-ompm-primary/10"
        aria-label="Cancel"
      >
        <X class="h-3 w-3" />
      </button>
      {#if addError}
        <span class="text-xs text-red-500 dark:text-red-400">{addError}</span>
      {/if}
    </div>
  {/if}

  <div class="flex-1 overflow-y-auto px-6 py-4">
    {#if sessionError}
      <div
        class="mb-3 flex items-start justify-between gap-3 rounded-md border border-red-500/40 bg-red-500/5 px-4 py-2 text-sm text-red-600 dark:text-red-400"
      >
        <span>{sessionError}</span>
        <button
          type="button"
          on:click={() => (sessionError = '')}
          class="shrink-0 text-red-600/70 hover:text-red-600 dark:text-red-400/70 dark:hover:text-red-400"
          aria-label="Dismiss error"
        >
          <X class="h-4 w-4" />
        </button>
      </div>
    {/if}
    {#if error}
      <div
        class="rounded-md border border-red-500/40 bg-red-500/5 px-4 py-3 text-sm text-red-600 dark:text-red-400"
      >
        {error}
      </div>
    {:else if loading && cards.length === 0}
      <p class="text-sm text-ompm-light-muted dark:text-ompm-text/60">Loading projects…</p>
    {:else if cards.length === 0}
      <p class="text-sm text-ompm-light-muted dark:text-ompm-text/60">
        {filter
          ? 'No projects match the filter.'
          : showAll
            ? 'No projects in registry.'
            : 'No active projects in the past 14 days.'}
      </p>
    {:else}
      <ul class="flex flex-col gap-3">
        {#each cards as project (project.id)}
          {@const ghPullsLink = githubUrl(project.git_origin, '/pulls')}
          {@const ghIssuesLink = githubUrl(project.git_origin, '/issues')}
          {@const ghLink = githubUrl(project.git_origin)}
          {@const age = relativeTime(project.last_active)}
          {@const activeCount = activeSessionCount(project)}
          {@const isOpen = expanded.has(project.id)}
          {@const recap = $recaps.get(project.id)}
          <li
            class="rounded-lg border border-ompm-light-border dark:border-ompm-border bg-ompm-light-surface dark:bg-ompm-surface shadow-sm overflow-hidden"
            title={recap ? `※ recap: ${recap.summary}` : undefined}
          >
            <button
              type="button"
              class="flex w-full items-start justify-between gap-3 px-4 py-3 text-left hover:bg-ompm-primary/5 transition-colors"
              on:click={() => toggleExpand(project.id)}
              aria-expanded={isOpen}
            >
              <div class="flex flex-1 flex-col gap-1 min-w-0">
                <div class="flex items-center gap-2 flex-wrap">
                  <ChevronRight
                    class="h-4 w-4 shrink-0 text-ompm-light-muted dark:text-ompm-text/60 transition-transform {isOpen
                      ? 'rotate-90'
                      : ''}"
                  />
                  <Folder class="h-4 w-4 shrink-0 text-ompm-primary" />
                  <span class="font-semibold text-ompm-light-text dark:text-ompm-text truncate">
                    {project.name}
                  </span>
                  {#if project.framework}
                    <span
                      class="rounded-full bg-ompm-primary/15 px-2 py-0.5 text-xs text-ompm-primary"
                    >
                      {project.framework}
                    </span>
                  {/if}
                  {#if activeCount > 0}
                    <span
                      class="inline-flex items-center gap-1 rounded-full bg-green-500/15 px-2 py-0.5 text-xs text-green-700 dark:text-green-300"
                      title="{activeCount} running session(s)"
                    >
                      <span class="inline-block h-1.5 w-1.5 rounded-full bg-green-500 session-pulse"></span>
                      {activeCount} active
                    </span>
                  {/if}
                  {#if project.open_prs_count != null && project.open_prs_count > 0}
                    {#if ghPullsLink}
                      <a
                        href={ghPullsLink}
                        target="_blank"
                        rel="noopener noreferrer"
                        on:click|stopPropagation
                        class="inline-flex items-center gap-1 rounded-full bg-ompm-amber/20 px-2 py-0.5 text-xs text-ompm-amber hover:bg-ompm-amber/30"
                        title="Open PRs on GitHub"
                      >
                        <GitPullRequest class="h-3 w-3" />
                        {project.open_prs_count}
                        {project.open_prs_count === 1 ? 'PR' : 'PRs'}
                      </a>
                    {:else}
                      <span
                        class="inline-flex items-center gap-1 rounded-full bg-ompm-amber/20 px-2 py-0.5 text-xs text-ompm-amber"
                      >
                        <GitPullRequest class="h-3 w-3" />
                        {project.open_prs_count}
                        {project.open_prs_count === 1 ? 'PR' : 'PRs'}
                      </span>
                    {/if}
                  {/if}
                  {#if project.open_issues_count != null && project.open_issues_count > 0}
                    {#if ghIssuesLink}
                      <a
                        href={ghIssuesLink}
                        target="_blank"
                        rel="noopener noreferrer"
                        on:click|stopPropagation
                        class="inline-flex items-center gap-1 rounded-full bg-red-500/15 px-2 py-0.5 text-xs text-red-700 dark:text-red-300 hover:bg-red-500/25"
                        title="Open issues on GitHub"
                      >
                        <Bug class="h-3 w-3" />
                        {project.open_issues_count}
                      </a>
                    {:else}
                      <span
                        class="inline-flex items-center gap-1 rounded-full bg-red-500/15 px-2 py-0.5 text-xs text-red-700 dark:text-red-300"
                      >
                        <Bug class="h-3 w-3" />
                        {project.open_issues_count}
                      </span>
                    {/if}
                  {/if}
                </div>
                <div class="flex items-center gap-2 flex-wrap text-xs text-ompm-light-muted dark:text-ompm-text/60">
                  {#if project.path}
                    <code class="font-mono truncate max-w-md" title={project.path}
                      >{project.path}</code
                    >
                  {/if}
                  {#if ghLink}
                    <span aria-hidden="true">·</span>
                    <a
                      href={ghLink}
                      target="_blank"
                      rel="noopener noreferrer"
                      on:click|stopPropagation
                      class="inline-flex items-center gap-1 text-ompm-primary hover:underline"
                    >
                      <Github class="h-3 w-3" />
                      {ghLink.replace('https://github.com/', '')}
                    </a>
                  {:else if project.git_origin}
                    <span aria-hidden="true">·</span>
                    <span class="font-mono">{project.git_origin}</span>
                  {/if}
                </div>
                <div class="text-xs text-ompm-light-muted dark:text-ompm-text/50">
                  Last connected: <span title={project.last_active ?? ''}>{age}</span>
                </div>
              </div>
            </button>

            {#if isOpen}
              <div
                class="border-t border-ompm-light-border dark:border-ompm-border px-4 py-3 bg-ompm-light-bg/40 dark:bg-ompm-bg/40"
              >
                {#if project.sessions && project.sessions.length > 0}
                  <ul class="flex flex-col gap-1">
                    {#each project.sessions as session (session.name)}
                      <li
                        class="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-ompm-primary/5 transition-colors"
                      >
                        <span
                          class="inline-block h-2 w-2 rounded-full shrink-0 {statusDotClasses(
                            session.status,
                          )}"
                          aria-hidden="true"
                        ></span>
                        <span
                          class="flex-1 truncate text-sm text-ompm-light-text dark:text-ompm-text font-mono"
                        >
                          {session.name}
                        </span>
                        <span
                          class="text-xs text-ompm-light-muted dark:text-ompm-text/60 font-mono"
                          >{session.adapter_type}</span
                        >
                        <span
                          class="rounded-full px-2 py-0.5 text-xs {statusPillClasses(
                            session.status,
                          )}"
                        >
                          {session.status}
                        </span>
                        <!-- #450: Attach (show tmux command), Pause/Resume, Kill -->
                        <button
                          type="button"
                          on:click={() => openAttachModal(session.name)}
                          class="inline-flex items-center justify-center rounded-md border border-ompm-light-border dark:border-ompm-border p-1 text-ompm-light-muted dark:text-ompm-text/60 hover:bg-ompm-primary hover:text-white hover:border-ompm-primary transition-colors"
                          title="Attach to session"
                          aria-label="Attach to session {session.name}"
                        >
                          <Link2 class="h-3 w-3" />
                        </button>
                        {#if session.status === 'Paused'}
                          <button
                            type="button"
                            on:click={() => resumeSession(session.name)}
                            class="inline-flex items-center justify-center rounded-md border border-ompm-light-border dark:border-ompm-border p-1 text-ompm-light-muted dark:text-ompm-text/60 hover:bg-green-500 hover:text-white hover:border-green-500 transition-colors"
                            title="Resume session"
                            aria-label="Resume session {session.name}"
                          >
                            <Play class="h-3 w-3" />
                          </button>
                        {:else if session.status === 'Running' || session.status === 'Idle'}
                          <button
                            type="button"
                            on:click={() => pauseSession(session.name)}
                            class="inline-flex items-center justify-center rounded-md border border-ompm-light-border dark:border-ompm-border p-1 text-ompm-light-muted dark:text-ompm-text/60 hover:bg-yellow-500 hover:text-white hover:border-yellow-500 transition-colors"
                            title="Pause session"
                            aria-label="Pause session {session.name}"
                          >
                            <Pause class="h-3 w-3" />
                          </button>
                        {/if}
                        <button
                          type="button"
                          on:click={() => killSession(session.name)}
                          class="inline-flex items-center justify-center rounded-md border border-ompm-light-border dark:border-ompm-border p-1 text-ompm-light-muted dark:text-ompm-text/60 hover:bg-red-500 hover:text-white hover:border-red-500 transition-colors"
                          title="Kill session"
                          aria-label="Kill session {session.name}"
                        >
                          <Trash2 class="h-3 w-3" />
                        </button>
                        <button
                          type="button"
                          on:click={() => connectToProject(project)}
                          class="inline-flex items-center justify-center rounded-md border border-ompm-light-border dark:border-ompm-border p-1 text-ompm-light-muted dark:text-ompm-text/60 hover:bg-ompm-primary hover:text-white hover:border-ompm-primary transition-colors"
                          title="Connect to chat"
                          aria-label="Connect to chat {session.name}"
                        >
                          <ArrowRight class="h-3 w-3" />
                        </button>
                      </li>
                    {/each}
                  </ul>
                {:else}
                  <p class="px-2 py-1 text-xs text-ompm-light-muted dark:text-ompm-text/60">
                    No sessions running.
                  </p>
                {/if}

                <button
                  type="button"
                  on:click={() => newSession(project)}
                  class="mt-2 inline-flex items-center gap-1 rounded-md border border-dashed border-ompm-light-border dark:border-ompm-border px-3 py-1.5 text-xs text-ompm-light-muted dark:text-ompm-text/60 hover:border-ompm-primary hover:text-ompm-primary transition-colors"
                >
                  <Plus class="h-3 w-3" />
                  New Session
                </button>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <!--
    #450: Attach modal. WebUI can't exec tmux directly (no PTY access from the
    browser), so we show the copy-pasteable `tmux attach-session -t <name>`
    command instead. The modal opens after "New Session" succeeds and on the
    per-session Attach button.
  -->
  {#if attachModalName}
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      role="dialog"
      aria-modal="true"
      aria-labelledby="attach-modal-title"
      on:click={closeAttachModal}
      on:keydown={(e) => e.key === 'Escape' && closeAttachModal()}
    >
      <div
        class="w-full max-w-md rounded-lg border border-ompm-light-border dark:border-ompm-border bg-ompm-light-surface dark:bg-ompm-surface p-5 shadow-xl"
        role="document"
        on:click|stopPropagation
        on:keydown|stopPropagation
      >
        <div class="mb-3 flex items-center justify-between">
          <h2
            id="attach-modal-title"
            class="text-base font-semibold text-ompm-light-text dark:text-ompm-text"
          >
            Connect to session: <span class="font-mono">{attachModalName}</span>
          </h2>
          <button
            type="button"
            on:click={closeAttachModal}
            class="text-ompm-light-muted dark:text-ompm-text/60 hover:text-ompm-light-text dark:hover:text-ompm-text"
            aria-label="Close"
          >
            <X class="h-4 w-4" />
          </button>
        </div>
        <p class="mb-2 text-xs text-ompm-light-muted dark:text-ompm-text/60">
          Run this in your terminal:
        </p>
        <pre
          class="mb-3 overflow-x-auto rounded-md border border-ompm-light-border dark:border-ompm-border bg-ompm-light-bg dark:bg-ompm-bg px-3 py-2 text-xs font-mono text-ompm-light-text dark:text-ompm-text"
        ><code>tmux attach-session -t {attachModalName}</code></pre>
        <div class="flex justify-end gap-2">
          <button
            type="button"
            on:click={copyAttachCommand}
            class="inline-flex items-center gap-1 rounded-md border border-ompm-light-border dark:border-ompm-border px-3 py-1.5 text-xs text-ompm-light-text dark:text-ompm-text hover:bg-ompm-primary/10"
          >
            <Copy class="h-3 w-3" />
            {attachCopied ? 'Copied!' : 'Copy command'}
          </button>
          <button
            type="button"
            on:click={closeAttachModal}
            class="rounded-md bg-ompm-primary px-3 py-1.5 text-xs text-white hover:bg-ompm-primary/90"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  /* Why: Tailwind's animate-pulse fades to 50% which still reads as "on";
     we want a stronger blink so live sessions stand out at a glance.
     Test: Inspect a Running session dot in DevTools and verify the opacity
     animates between 1 and 0.3 over a 1.5s cycle. */
  @keyframes ompm-pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.3;
    }
  }
  :global(.session-pulse) {
    animation: ompm-pulse 1.5s ease-in-out infinite;
  }
</style>
