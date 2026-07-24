<script lang="ts">
  /**
   * Why (#3819, epic #3052): Bob's directive replaces the `PROJECTS` section
   * (one static `CTRL` entry) with a single user-facing `TASKS` section —
   * internally still "workstreams" (API/tagging/spec vocabulary unchanged;
   * "Tasks" is a presentation-layer rename only) — sourced from trusty-
   * memory's `ws:<name>` tag convention (DOC-53, the same one the `tm`
   * harness uses) and grouped/collapsible by owning agent. Per Bob's
   * follow-up review of the running build: the sidebar carries NO agent
   * shortcut at all — agent selection happens ONLY via `ChatHeader`'s
   * selector, so the Concierge pin that used to live here (mirroring the
   * old Projects/CTRL slot) is REMOVED, not relabeled. The separate "Recent
   * tasks" section (`TaskHistory.svelte`, backend `GET /api/tasks` workflow-
   * run history) is also removed — Bob: one "Tasks" section, no duplication.
   * `TaskHistory.svelte` is left in the tree, unrouted (same treatment as
   * `ProjectsView`/`PersonalityPanel`) rather than deleted, since it's real,
   * tested, backend-integrated functionality (live status/cost/score per
   * run) with no current call site — folding its unique info into the
   * memory-tagged Tasks rows is a follow-up, not done ad hoc under time
   * pressure. Also drops the standalone "Clear Context" footer button per
   * Bob (superseded by "+ New Task").
   *
   * IMPORTANT model note (Bob's continuous-per-agent-chat refinement): there
   * is ONE ongoing chat per agent, no per-task context isolation — the
   * agent infers/classifies tasks from the conversation, and this Tasks
   * list is a FILTER/VIEW over that one stream, not a separate context.
   * "+ New Task" (below, `startNewTask`) reflects this NOW: it inserts a
   * topic-boundary divider into the stream rather than calling the old
   * `POST /api/clear-context` hard-reset — no network call, nothing
   * aborted. NOTE: this removes the prior escape hatch for unsticking a
   * genuinely stuck/errored session via this button; a page reload remains
   * available. The sidebar Tasks list itself is still a separate
   * server-sourced list (`GET /api/workstreams`), not yet literally a view
   * over the live message stream — reconciling that fully is a larger
   * follow-up flagged in the PR body. Task *resume* (`resumeWorkstream`
   * below) is unaffected — "inject recent tagged history as a context
   * banner" remains valid under this model.
   */
  import { onMount } from 'svelte';
  import { Loader2, Plus, ChevronRight, ChevronDown } from 'lucide-svelte';
  import {
    activeAgentId,
    agentRoster,
    addMessage,
    fetchAgentCatalog,
  } from '../stores/app';
  import { fetchWorkstreams, fetchWorkstreamHistory, groupByAgent, type AgentGroup } from '../lib/workstreams';
  import LogoMark from '../lib/icons/LogoMark.svelte';

  export let apiReady = false;
  export let apiError = '';

  let workstreamGroups: AgentGroup[] = [];
  let loadingWorkstreams = true;
  let collapsed = new Set<string>();
  let resumingName: string | null = null;

  function toggleGroup(agentId: string) {
    const next = new Set(collapsed);
    if (next.has(agentId)) next.delete(agentId);
    else next.add(agentId);
    collapsed = next;
  }

  async function loadWorkstreams() {
    loadingWorkstreams = true;
    const [workstreams] = await Promise.all([fetchWorkstreams(), fetchAgentCatalog()]);
    // #3819: exclude `ctrl` from the grouping roster the same way
    // `ChatHeader` does — `resumeWorkstream` below sets `activeAgentId` to
    // a matched group's id, and `activeAgentId = 'ctrl'` would dispatch
    // through the tools-OFF persona-chat path instead of Concierge's
    // tools-armed one (see `ChatHeader.svelte`'s doc comment for the full
    // load-bearing explanation). A workstream that happens to match "ctrl"
    // by name substring falls into the "Other" bucket instead.
    const knownAgents = $agentRoster
      .filter((e) => e.id !== 'ctrl')
      .map((e) => ({ id: e.id, label: e.label }));
    workstreamGroups = groupByAgent(workstreams, knownAgents);
    loadingWorkstreams = false;
  }

  /**
   * Why: "Resume" for this first slice (#3819 issue body) means injecting
   * the task's recent tagged history as a context banner into the active
   * chat, and switching the active persona to the task's owning agent (when
   * it's a real roster match, not the "Other" bucket) — NOT tagging new
   * outgoing turns with the workstream (that needs a workstream-attribution
   * concept trusty-agents' own conversation memory doesn't have yet;
   * documented as a follow-up).
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
        content: `Resumed task "${name}" — recent tagged memory:\n${digest}`,
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
   * Why (Bob's continuous-per-agent-chat model, received after this
   * sidebar's first pass — see the module doc's "IMPORTANT model note"):
   * there is ONE ongoing chat per agent, no hard context wall between
   * tasks. "+ New Task" therefore must NOT clear/abort anything — it marks
   * a topic boundary in the stream (a divider row, `ChatView.svelte`'s
   * `topic-boundary` role) that the agent can use to classify/segment
   * turns, nothing more.
   * What: Appends a `topic-boundary` message to the `ctrl` thread. No
   * network call, no page reload, no task abort. NOTE: this deliberately
   * removes the prior hard-reset escape hatch for a stuck/errored session
   * (previously `POST /api/clear-context` via this same button) — flagged
   * as an accepted demo-scope gap in the PR body; a page reload still resets
   * client-side state if needed.
   * Test: Manual — click "+ New Task", confirm a divider appears in the
   * chat stream and no network request fires.
   */
  function startNewTask() {
    addMessage('ctrl', {
      id: `topic-boundary-${Date.now()}`,
      role: 'topic-boundary',
      content: 'New task',
      timestamp: Date.now(),
    });
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

  <!-- #3222/#3819: "+ NEW TASK" — Foundry mockup (docs/design/gui/Foundry
       Ecosystem.dc.html:167), full-width rectangular button above TASK
       HISTORY. Non-destructive: inserts a topic-boundary divider
       (`startNewTask`) rather than clearing context — see the module doc's
       "IMPORTANT model note". -->
  <div class="px-3 pt-3 pb-1">
    <button
      type="button"
      class="flex w-full items-center justify-center gap-1.5 rounded-md border border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary dark:bg-foundry-primary px-3 py-2 text-xs font-semibold uppercase tracking-wide text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80"
      on:click={startNewTask}
    >
      <Plus class="h-3.5 w-3.5" />
      New Task
    </button>
  </div>

  <!-- #3819: "TASKS" is the user-facing label for workstreams (Bob:
       "less technical" — the API/tagging/spec vocabulary stays
       'workstream'; this is presentation-layer only). Grouped/collapsible
       by owning agent; each row resumable. The ONLY tasks section (no
       separate "Recent tasks" / TaskHistory below it — Bob: one list, no
       duplication) and the sidebar carries NO agent shortcut (agent
       selection lives solely in ChatHeader). -->
  <div class="flex flex-1 flex-col gap-1 overflow-y-auto border-t border-foundry-light-border dark:border-foundry-border px-2 py-3">
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

  <!-- #3819: the standalone "Clear Context" footer button is removed per
       Bob — "+ New Task" (above) covers it and doubles as "start a new
       task" (fresh context). -->
</aside>
