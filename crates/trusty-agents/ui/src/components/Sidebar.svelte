<script lang="ts">
  /**
   * Why (#3819, epic #3052): Bob's directive replaces the `PROJECTS` section
   * (one static `CTRL` entry) with user-facing `TASKS` — internally still
   * "workstreams" (API/tagging/spec vocabulary unchanged; "Tasks" is a
   * presentation-layer rename only) — sourced from trusty-memory's
   * `ws:<name>` tag convention (DOC-53, the same one the `tm` harness uses)
   * and grouped/collapsible by owning agent. CTRL's prior "select the ctrl
   * conversation" capability is preserved as a pinned `Concierge` button
   * above the list — see `ChatHeader.svelte`'s doc comment for how
   * "Concierge" maps onto the pre-existing `activeAgentId = null` dispatch
   * path. Also drops the standalone "Clear Context" footer button per Bob
   * (superseded by "+ New Task").
   *
   * IMPORTANT model note (Bob's later refinement, received after this
   * component's first pass — flagged in the PR body as NOT fully
   * implemented in this slice): the intended end-state is ONE continuous
   * per-agent chat with no per-task context isolation — the agent infers
   * and classifies tasks from the conversation/events, and the sidebar
   * Tasks list is meant to be a FILTER/VIEW over that one stream, not a
   * separate context. "+ New Task" is meant to be a classification hint,
   * NOT a context wipe. What ships in THIS slice still matches the
   * pre-existing app architecture: `handleClearContext` (bound to "+ New
   * Task" below) still calls `POST /api/clear-context`, which really does
   * clear the task/chat store — reconciling that to "hint, not wipe"
   * requires touching the backend clear-context endpoint and the
   * project-keyed message-store model, out of scope for this pass. Task
   * resume (`resumeWorkstream` below) still works as "inject recent tagged
   * history as a context banner," which remains valid under either model.
   */
  import { onMount } from 'svelte';
  import { Terminal, Loader2, Plus, ChevronRight, ChevronDown } from 'lucide-svelte';
  import { apiBase } from '../lib/api-config';
  import {
    activeAgentId,
    agentRoster,
    isRunning,
    getCurrentApiToken,
    addMessage,
    fetchAgentCatalog,
  } from '../stores/app';
  import { fetchWorkstreams, fetchWorkstreamHistory, groupByAgent, type AgentGroup } from '../lib/workstreams';
  import TaskHistory from './TaskHistory.svelte';
  import LogoMark from '../lib/icons/LogoMark.svelte';

  export let apiReady = false;
  export let apiError = '';

  let clearing = false;
  let workstreamGroups: AgentGroup[] = [];
  let loadingWorkstreams = true;
  let collapsed = new Set<string>();
  let resumingName: string | null = null;

  function selectConcierge() {
    activeAgentId.set(null);
  }

  function toggleGroup(agentId: string) {
    const next = new Set(collapsed);
    if (next.has(agentId)) next.delete(agentId);
    else next.add(agentId);
    collapsed = next;
  }

  async function loadWorkstreams() {
    loadingWorkstreams = true;
    const [workstreams] = await Promise.all([fetchWorkstreams(), fetchAgentCatalog()]);
    const knownAgents = $agentRoster
      .filter((e) => e.source !== 'base')
      .map((e) => ({ id: e.id, label: e.label }));
    workstreamGroups = groupByAgent(workstreams, knownAgents);
    loadingWorkstreams = false;
  }

  /**
   * Why: "Resume" for this first slice (#3819 issue body) means injecting
   * the workstream's recent tagged history as a context banner into the
   * active chat, and switching the active persona to the workstream's
   * owning agent (when it's a real roster match, not the "Other" bucket) —
   * NOT tagging new outgoing turns with the workstream (that needs a
   * workstream-attribution concept trusty-agents' own conversation memory
   * doesn't have yet; documented as a follow-up).
   * What: Fetches history, formats a compact digest, appends it as a
   * `system`-role banner message to the `ctrl` thread (the only thread this
   * app has), and sets `activeAgentId` when the group is a known agent.
   */
  async function resumeWorkstream(name: string, group: AgentGroup) {
    resumingName = name;
    try {
      const history = await fetchWorkstreamHistory(name, 10);
      const digest = history.length
        ? history
            .map((h) => `- ${h.content.split('\n')[0].slice(0, 140)}`)
            .join('\n')
        : '(no additional tagged history found)';
      addMessage('ctrl', {
        id: `workstream-resume-${name}-${Date.now()}`,
        role: 'system',
        content: `Resumed workstream "${name}" — recent tagged memory:\n${digest}`,
        timestamp: Date.now(),
      });
      if (group.agentId !== 'other') {
        activeAgentId.set(group.agentId);
      }
    } finally {
      resumingName = null;
    }
  }

  /**
   * Why: `POST /api/clear-context` now aborts any in-flight task as of
   * #3196. "+ NEW TASK" is the only entry point for it now (#3819 dropped
   * the separate "Clear Context" footer button) and doubles as "start a new
   * workstream" — fresh context, no separate create ceremony.
   * What: Returns true immediately when nothing is running; otherwise shows
   * a native `confirm()` and returns the user's choice.
   * Test: Manual — start a task, click "+ New Task", confirm the dialog
   * appears and Cancel leaves the task running.
   */
  function confirmIfRunning(): boolean {
    if (!$isRunning) return true;
    return confirm('A task is currently running. This will stop it and clear the chat. Continue?');
  }

  /**
   * Why: Lets users wipe accumulated task history and in-flight sessions
   * without restarting the server — a common need during iterative
   * development, and (#3222) the target of the "+ NEW TASK" button, which
   * per #3819 is now this app's ONLY "start fresh" affordance (implicitly:
   * start a new workstream).
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
    loadWorkstreams();
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
    <!-- #3819: Concierge (ctrl) pinned above the TASKS list — not itself a
         task, the fixed coordination-layer thread. -->
    <button
      type="button"
      class="flex items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition-colors {$activeAgentId === null
        ? 'bg-foundry-light-primary/20 dark:bg-foundry-primary/20 text-foundry-light-text dark:text-foundry-text border-l-2 border-foundry-light-primary dark:border-foundry-primary'
        : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
      on:click={selectConcierge}
    >
      <Terminal class="h-4 w-4" />
      <span class="flex-1 truncate">Concierge</span>
    </button>
  </nav>

  <!-- #3819: "TASKS" is the user-facing label for workstreams (Bob:
       "less technical" — the API/tagging/spec vocabulary stays
       'workstream'; this is presentation-layer only). Grouped/collapsible
       by owning agent; each row resumable. -->
  <div class="flex flex-col gap-1 border-t border-foundry-light-border dark:border-foundry-border px-2 py-3">
    <h2 class="mb-1 px-2 text-xs font-semibold uppercase tracking-wide text-foundry-teal">
      Tasks
    </h2>
    {#if loadingWorkstreams}
      <div class="flex items-center gap-2 px-3 py-1.5 text-xs text-foundry-light-muted dark:text-foundry-text/50">
        <Loader2 class="h-3 w-3 animate-spin" /> Loading…
      </div>
    {:else if workstreamGroups.length === 0}
      <p class="px-3 py-1.5 text-xs text-foundry-light-muted dark:text-foundry-text/40">
        No resumable tasks yet — they appear here once trusty-memory has tagged activity for this
        project.
      </p>
    {:else}
      {#each workstreamGroups as group (group.agentId)}
        <div>
          <button
            type="button"
            class="flex w-full items-center gap-1 px-2 py-1 text-left font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50 hover:text-foundry-light-text dark:hover:text-foundry-text"
            on:click={() => toggleGroup(group.agentId)}
          >
            {#if collapsed.has(group.agentId)}
              <ChevronRight class="h-3 w-3" />
            {:else}
              <ChevronDown class="h-3 w-3" />
            {/if}
            {group.agentLabel}
            <span class="ml-auto normal-case tracking-normal text-foundry-light-muted/70 dark:text-foundry-text/30">{group.workstreams.length}</span>
          </button>
          {#if !collapsed.has(group.agentId)}
            {#each group.workstreams as ws (ws.name)}
              <button
                type="button"
                class="flex w-full items-center gap-2 rounded-md px-3 py-1.5 text-left text-xs text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 disabled:opacity-50"
                disabled={resumingName === ws.name}
                title={ws.summary}
                on:click={() => resumeWorkstream(ws.name, group)}
              >
                <span
                  class="inline-block h-1.5 w-1.5 shrink-0 rounded-full {ws.has_open_claim
                    ? 'bg-foundry-amber'
                    : 'bg-foundry-light-muted/40 dark:bg-foundry-text/30'}"
                ></span>
                <span class="flex-1 truncate">{ws.name}</span>
                {#if resumingName === ws.name}
                  <Loader2 class="h-3 w-3 shrink-0 animate-spin" />
                {/if}
              </button>
            {/each}
          {/if}
        </div>
      {/each}
    {/if}
  </div>

  <div class="flex-1 overflow-y-auto border-t border-foundry-light-border dark:border-foundry-border px-2 py-3">
    <TaskHistory />
  </div>
  <!-- #3819: the standalone "Clear Context" footer button is removed per
       Bob — "+ New Task" (above) covers it and doubles as "start a new
       task" (fresh context). -->
</aside>
