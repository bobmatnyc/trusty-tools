<script lang="ts">
  // Why: DOC-39 shell rebuild (issue #3153) — the "workstream context"
  // scope's tab bar (§2), the 7 CANONICAL tabs the build brief names from
  // mockups 9a/9b/10d/10e/11b (Workstream · Project · Agents · Memory ·
  // Search · Workflow · Files), explicitly superseding mockup 8a's stale
  // 5-tab draft. All visibility/lock decisions are delegated to the pure
  // `tabVisibility` helper (`lib/nav-tabs.ts`) — see that module's doc
  // comment for the AC-4.2 ("locked, not hidden") / AC-9.4 (Workflow
  // "hidden, not locked") rules this component only renders, never decides.
  //
  // Live dots (AC-6.1, rung 1 of the §4.6 progressive-disclosure ladder)
  // are wired for the one tab with an existing pollable signal in Phase 1 —
  // Workstream, reflecting whether the active session is still running.
  // Every other tab's dot slot is reserved but dark: no other tab has a
  // wired live/settled signal yet (the SSE consumer work is #3251/#3252,
  // explicitly deferred by the build brief), so rendering a dot there would
  // fabricate liveness the daemon hasn't reported.
  //
  // What: Renders one button per visible tab (per `tabVisibility`), calling
  // `onSelect` on click; a locked tab is `disabled` with an explanatory
  // `title` rather than omitted; the active tab gets the Foundry "active
  // nav" rust treatment (role mapping, DOC-39 §8: one rust accent per
  // region).
  // Test: `nav-tabs.test.ts` covers the visibility logic this component
  // renders; `App.test.ts` exercises tab switching end-to-end through this
  // component's buttons.
  import { NAV_TABS, tabVisibility, type TabId } from '../lib/nav-tabs';

  let {
    activeTab,
    hasProject,
    isGitRepo = false,
    sessionRunning = false,
    onSelect,
  }: {
    activeTab: TabId;
    hasProject: boolean;
    isGitRepo?: boolean;
    sessionRunning?: boolean;
    onSelect: (tab: TabId) => void;
  } = $props();

  let entries = $derived(
    NAV_TABS.map((tab) => ({ tab, visibility: tabVisibility(tab.id, hasProject, isGitRepo) })).filter(
      (e) => e.visibility.visible,
    ),
  );
</script>

<nav class="wsnav flex shrink-0 items-center gap-1 border-b border-trusty-border bg-trusty-card px-2">
  {#each entries as { tab, visibility } (tab.id)}
    <button
      type="button"
      disabled={visibility.locked}
      title={visibility.locked ? `${tab.label} — locked until a project is bound` : tab.label}
      onclick={() => onSelect(tab.id)}
      class={`flex items-center gap-1.5 border-b-2 px-3 py-2 font-mono text-[11px] font-semibold uppercase tracking-wide disabled:cursor-not-allowed disabled:opacity-40 ${
        activeTab === tab.id
          ? 'border-trusty-primary text-trusty-primary'
          : 'border-transparent text-trusty-text-secondary hover:text-trusty-text'
      }`}
    >
      {#if tab.id === 'workstream' && sessionRunning}
        <span class="h-1.5 w-1.5 rounded-full bg-status-ok" title="session running"></span>
      {/if}
      {tab.label}
    </button>
  {/each}
</nav>
