<script lang="ts">
  // Why: DOC-39 shell rebuild (issue #3153) — the `.wsrail`, a segmented
  // Workstream|Project toggle over a card list, distinct from the
  // `ServiceNav` tab bar above it: the rail picks WHICH item the active
  // pane shows within a scope, the nav picks WHICH SERVICE. 240px wins over
  // the Explainer PDF's 236px (Foundry's own `--trusty-sidebar-width`
  // token, docs/design/UI/design-system/tokens.css) per the build brief's
  // contradiction ruling.
  //
  // **Issue #3447 makes the Project view a real, primary project picker.**
  // Bob: "Projects list in left pane." Phase 1 originally had no
  // workstream/project registry, so this view was a single honest stub
  // ("+ add project", handing off to `StartWorkingForm`'s own modal
  // picker). Issue #3439 shipped the shared-registry-backed roster
  // (`lib/project-roster.ts`, `GET /projects`) that `ProjectPickerModal`
  // already consumes — this view now fetches and renders that SAME roster
  // directly (its own independent poll-on-become-visible, the established
  // house convention every component here follows), so browsing/picking a
  // project no longer requires leaving the rail to open a modal. The modal
  // remains — reachable via `StartWorkingForm`'s "choose project" button —
  // as genuinely secondary access: quick reselection from the input bar
  // without moving focus away from typing, not a second copy of the same
  // primary surface.
  //
  // **Selection persistence + continuation are one state model (issue
  // #3447 bug 1 + issue #3446).** Clicking a roster row (or the
  // "projectless" row) writes to the SAME shared cross-module store
  // (`lib/selected-project.svelte.ts`) `StartWorkingForm`'s own picker
  // writes to — there is exactly one selection, regardless of which surface
  // changed it. `StartWorkingForm.svelte`'s own `$effect` (watching that
  // store) is what resets chat-continuation state on a genuine change; this
  // component does not need its own copy of that logic. Also switches the
  // active tab to Workstream on a pick (`onSwitchToWorkstream`, an existing
  // prop this view previously used only for its stub button) — "click =
  // select/switch active project" (issue #3447's own wording) reads most
  // naturally as "and now go type," not merely "the variable changed
  // somewhere off-screen."
  //
  // Phase 1 still has no workstream registry (§6.3/7B, #3187 recents list,
  // clone-from-URL unbuilt) — the Workstream view keeps rendering the one
  // synthetic "current session" entry when a session is active (7b); that
  // half is UNCHANGED by this issue.
  //
  // What: A collapsible (240px / 46px via the `«`/`»` toggle) rail: a
  // segmented `railView` toggle (hidden while collapsed), then the
  // Workstream or Project card list for whichever view is selected. The
  // Project view fetches `GET /projects` (via [`fetchProjectRoster`])
  // whenever it becomes visible (`railView === 'project'` AND not
  // collapsed) — same `AbortController`-per-effect shape
  // `ProjectPickerModal.svelte` already established for the identical
  // fetch, mirrored here rather than shared, since the two surfaces have
  // independent visibility lifecycles. Renders the same three phases
  // (`loading`/`daemon-unreachable`/`ready`) and the same `fs_only` banner
  // (issue #3435, code-critic PR #3439 review HIGH 2) that modal already
  // established. A roster row highlights when it matches the shared
  // selection's `path`; the "projectless" row highlights when the shared
  // selection is `null`.
  // Test: `WorkstreamRail.test.ts` covers the collapse toggle's width
  // class, the `railView` segmented toggle swapping list content, the
  // Workstream view's active-session-present/absent rendering, the Project
  // view's roster fetch/render/loading/daemon-unreachable/fs_only-banner
  // phases, row-click selecting into the shared store AND switching tabs,
  // and the projectless row.
  import { apiBase } from '../lib/api-config';
  import {
    fetchProjectRoster,
    projectCandidateLabel,
    type ProjectCandidate,
    type RosterSource,
  } from '../lib/project-roster';
  import { selectedProjectState, selectProject } from '../lib/selected-project.svelte';
  import type { SessionSummary } from '../lib/session-status';

  let {
    collapsed,
    onToggleCollapse,
    activeSession = null,
    onSwitchToWorkstream,
  }: {
    collapsed: boolean;
    onToggleCollapse: () => void;
    activeSession?: SessionSummary | null;
    onSwitchToWorkstream: () => void;
  } = $props();

  type RailView = 'workstream' | 'project';
  let railView = $state<RailView>('workstream');

  type ProjectPhase = 'loading' | 'daemon-unreachable' | 'ready';
  let projectPhase = $state<ProjectPhase>('loading');
  let projectEntries = $state<ProjectCandidate[]>([]);
  let projectSource = $state<RosterSource | undefined>(undefined);
  let projectError = $state<string | null>(null);

  async function loadProjects(signal: AbortSignal) {
    projectPhase = 'loading';
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        projectPhase = 'daemon-unreachable';
        projectError = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    try {
      const roster = await fetchProjectRoster(base, signal);
      if (signal.aborted) return;
      projectEntries = roster.entries;
      projectSource = roster.source;
      projectPhase = 'ready';
      projectError = null;
    } catch (e) {
      if (!signal.aborted) {
        projectPhase = 'daemon-unreachable';
        projectError = e instanceof Error ? e.message : String(e);
      }
    }
  }

  $effect(() => {
    if (collapsed || railView !== 'project') return;
    const controller = new AbortController();
    void loadProjects(controller.signal);
    return () => controller.abort();
  });

  function pickProject(entry: ProjectCandidate) {
    selectProject({ path: entry.path, displayPath: projectCandidateLabel(entry), isGitRepo: true });
    onSwitchToWorkstream();
  }

  function pickProjectless() {
    selectProject(null);
    onSwitchToWorkstream();
  }
</script>

<aside
  class={`wsrail flex shrink-0 flex-col border-r border-trusty-sidebar-border bg-trusty-sidebar-bg text-trusty-sidebar-text transition-[width] ${
    collapsed ? 'w-[46px]' : 'w-60'
  }`}
>
  <div class="flex items-center justify-end p-1.5">
    <button
      type="button"
      onclick={onToggleCollapse}
      aria-label={collapsed ? 'expand rail' : 'collapse rail'}
      title={collapsed ? 'expand' : 'collapse'}
      class="rounded-sm px-1.5 py-0.5 font-mono text-xs text-trusty-sidebar-muted hover:text-trusty-sidebar-text"
    >
      {collapsed ? '»' : '«'}
    </button>
  </div>

  {#if !collapsed}
    <div class="flex gap-1 px-2 pb-2">
      <button
        type="button"
        onclick={() => (railView = 'workstream')}
        class={`flex-1 rounded-sm px-2 py-1 font-mono text-[10px] font-semibold uppercase tracking-wide ${
          railView === 'workstream'
            ? 'bg-trusty-sidebar-active text-trusty-sidebar-text'
            : 'text-trusty-sidebar-muted hover:text-trusty-sidebar-text'
        }`}
      >
        workstreams
      </button>
      <button
        type="button"
        onclick={() => (railView = 'project')}
        class={`flex-1 rounded-sm px-2 py-1 font-mono text-[10px] font-semibold uppercase tracking-wide ${
          railView === 'project'
            ? 'bg-trusty-sidebar-active text-trusty-sidebar-text'
            : 'text-trusty-sidebar-muted hover:text-trusty-sidebar-text'
        }`}
      >
        projects
      </button>
    </div>

    <div class="flex-1 space-y-1.5 overflow-y-auto px-2 pb-2">
      {#if railView === 'workstream'}
        {#if activeSession}
          <div
            class="rounded-sm border-l-4 border-status-warn bg-trusty-sidebar-active px-2 py-1.5"
            title={activeSession.id}
          >
            <p class="font-mono text-[11px] font-semibold uppercase tracking-wide">
              {activeSession.status}
            </p>
            <p class="truncate text-[11px] text-trusty-sidebar-muted">
              {activeSession.project ?? 'projectless'}
            </p>
          </div>
        {:else}
          <p class="px-1 py-2 text-[11px] text-trusty-sidebar-muted">
            no active workstream yet — start working from the Workstream tab
          </p>
        {/if}
      {:else if projectPhase === 'loading'}
        <p class="px-1 py-2 text-[11px] text-trusty-sidebar-muted">loading…</p>
      {:else if projectPhase === 'daemon-unreachable'}
        <p class="px-1 py-2 text-[11px] text-status-error">
          daemon unreachable{projectError ? ` — ${projectError}` : ''}
        </p>
      {:else}
        {#if projectSource === 'fs_only'}
          <p class="rounded-sm bg-status-warn/10 px-2 py-1.5 text-[10px] text-status-warn">
            shared registry unavailable — showing local checkouts only
          </p>
        {/if}

        {#if projectEntries.length === 0}
          <p class="px-1 py-2 text-[11px] text-trusty-sidebar-muted">no known projects found</p>
        {:else}
          {#each projectEntries as entry (entry.path)}
            <button
              type="button"
              title={entry.path}
              onclick={() => pickProject(entry)}
              class={`flex w-full items-center justify-between gap-2 truncate rounded-sm px-2 py-1.5 text-left font-mono text-[11px] ${
                selectedProjectState.project?.path === entry.path
                  ? 'bg-trusty-sidebar-active text-trusty-sidebar-text'
                  : 'text-trusty-sidebar-muted hover:text-trusty-sidebar-text'
              }`}
            >
              <span class="truncate">{projectCandidateLabel(entry)}</span>
              {#if !entry.registered}
                <span class="shrink-0 text-[10px] uppercase tracking-wide">local only</span>
              {/if}
            </button>
          {/each}
        {/if}

        <button
          type="button"
          onclick={pickProjectless}
          class={`w-full rounded-sm border-1.5 border-dashed px-2 py-1.5 text-center font-mono text-[11px] uppercase tracking-wide ${
            selectedProjectState.project === null
              ? 'border-trusty-sidebar-active text-trusty-sidebar-text'
              : 'border-trusty-sidebar-border text-trusty-sidebar-muted hover:text-trusty-sidebar-text'
          }`}
        >
          projectless (chat only)
        </button>
      {/if}
    </div>
  {/if}
</aside>
