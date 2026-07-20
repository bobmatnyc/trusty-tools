<script lang="ts">
  // Why: DOC-39 shell rebuild (issue #3153) — the `.wsrail`, a segmented
  // Workstream|Project toggle over a card list, distinct from the
  // `ServiceNav` tab bar above it: the rail picks WHICH item the active
  // pane shows within a scope, the nav picks WHICH SERVICE. 240px wins over
  // the Explainer PDF's 236px (Foundry's own `--trusty-sidebar-width`
  // token, docs/design/UI/design-system/tokens.css) per the build brief's
  // contradiction ruling.
  //
  // Phase 1 has no workstream/project registry yet (§6.3/7B, #3187 for the
  // recents list, clone-from-URL unbuilt) — both list views render a single
  // honest stub rather than fabricated rows: Workstream shows the one
  // synthetic "current session" entry when a session is active (7b);
  // Project shows only the dashed "+ Add project" affordance (7a), which
  // just switches to the Workstream tab where `StartWorkingForm`'s picker
  // already lives (no separate picker is built here — one picker, one
  // owner).
  //
  // What: A collapsible (240px / 46px via the `«`/`»` toggle) rail: a
  // segmented `railView` toggle (hidden while collapsed), then the
  // Workstream or Project card list for whichever view is selected. The
  // Project view's "+ add project" card calls `onSwitchToWorkstream` — it
  // does not run its own picker (one picker, one owner, per the Why block
  // above); `App.svelte` wires that callback to `activeTab = 'workstream'`.
  // Test: `WorkstreamRail.test.ts` covers the collapse toggle's width
  // class, the `railView` segmented toggle swapping list content, the
  // Workstream view's active-session-present/absent rendering, and the
  // add-project button invoking `onSwitchToWorkstream`; `App.test.ts` pins
  // the `.wsrail` sibling position in the shell skeleton.
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
      {:else}
        <button
          type="button"
          onclick={onSwitchToWorkstream}
          title="Browse a project folder from the Workstream tab's picker — recents (#3187) and clone-from-URL are stub"
          class="w-full rounded-sm border-1.5 border-dashed border-trusty-sidebar-border px-2 py-2 text-center font-mono text-[11px] text-trusty-sidebar-muted hover:text-trusty-sidebar-text"
        >
          + add project
        </button>
      {/if}
    </div>
  {/if}
</aside>
