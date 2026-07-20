<script lang="ts">
  // Why: DOC-39 §8's Foundry shell skeleton (issue #3153 shell rebuild,
  // build brief archived at
  // https://github.com/bobmatnyc/trusty-tools/issues/3153#issuecomment-5015805597)
  // — the prior scaffold (#2983) was a single flat `.body` stack of cards.
  // This is the STRUCTURAL rebuild the brief scopes: the Foundry retheme
  // (PR #3197) already landed the visual system; what was still a flat
  // stack becomes the real one-flex-column skeleton — `.hdr` → `.body`
  // (`.wsrail` + `.actpane` = `.wsnav` + `.actbody`) → `.statusbar` —
  // DOC-39 §8.1/AC-18.1's nesting invariant (`.statusbar` a SIBLING of
  // `.body`, never inside it) is carried forward verbatim; it is the one
  // piece of this skeleton with a pinned regression test and has regressed
  // twice before.
  //
  // `HealthPanel` is REMOVED per the build brief ("no mockup home; fold
  // daemon-unreachable signal into status bar") — `StatusBar`'s own
  // `phase === 'daemon-unreachable'` branch already reports the identical
  // signal (a failed `GET /sessions` call), so the smoke-test panel was a
  // second surface for the same fact, not a distinct one.
  //
  // `hasProject`/`isGitRepo` drive `ServiceNav`'s AC-4.2 ("locked, not
  // hidden") / AC-9.4 (Workflow "hidden, not locked") rules. Phase 1 has no
  // workstream/project registry (§6.3/7B) or `project.status` aggregation
  // (#3181) — the only daemon-owned fact available today is the active
  // session's own `project` field (`GET /sessions`, already polled
  // elsewhere in this codebase 3x over for the identical reason: each
  // component polls independently, the established house convention). No
  // wire field carries git-repo-ness for an EXISTING session (only
  // `CreateSessionForm`'s pre-submit directory listing has `is_git_repo`,
  // and that's ephemeral, pre-binding, client-only state) — `isGitRepo` is
  // therefore always `false` here until a real `project.status`-style route
  // exists, which per AC-9.4 keeps Workflow hidden in Phase 1 rather than
  // guessing. This is a real API gap, not a UI shortcut (DOC-39 §2.1 C-2).
  //
  // What: `.app` renders `AppHeader`, then `.body` (`WorkstreamRail` +
  // `.actpane` = `ServiceNav` + `.actbody` hosting the selected tab's
  // component), then `StatusBar` as `.body`'s sibling. A single `$effect`
  // polls `GET /sessions` (same `AbortController`-per-lifetime shape as
  // every other poller in this codebase) purely to derive `hasProject`/
  // `activeSession`/`sessionRunning` for the nav and rail — it renders
  // nothing of its own.
  // Test: `App.test.ts` pins the `.statusbar`-sibling-of-`.body` DOM
  // invariant (AC-18.1, unchanged), the default Workstream tab's content,
  // and tab-switching to Search through `ServiceNav`'s buttons.
  import AppHeader from './components/AppHeader.svelte';
  import ServiceNav from './components/ServiceNav.svelte';
  import WorkstreamRail from './components/WorkstreamRail.svelte';
  import WorkstreamTab from './components/WorkstreamTab.svelte';
  import ProjectTab from './components/ProjectTab.svelte';
  import AgentsTab from './components/AgentsTab.svelte';
  import SkillsTab from './components/SkillsTab.svelte';
  import MemoryTab from './components/MemoryTab.svelte';
  import SearchTab from './components/SearchTab.svelte';
  import WorkflowTab from './components/WorkflowTab.svelte';
  import FilesTab from './components/FilesTab.svelte';
  import StatusBar from './components/StatusBar.svelte';
  import { apiBase } from './lib/api-config';
  import {
    pickActiveSession,
    TERMINAL_SESSION_STATUSES,
    type SessionListResponse,
    type SessionSummary,
  } from './lib/session-status';
  import type { TabId } from './lib/nav-tabs';

  const POLL_MS = 5000;

  let activeTab = $state<TabId>('workstream');
  let railCollapsed = $state(false);
  let activeSession = $state<SessionSummary | null>(null);

  // See the module doc above: no wire field carries git-repo-ness for an
  // existing session yet, so this stays `false` until a real API exists.
  const isGitRepo = false;

  async function refreshNavState(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch {
      if (!signal.aborted) activeSession = null;
      return;
    }
    if (signal.aborted) return;

    try {
      const res = await fetch(`${base}/sessions`, { signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as SessionListResponse;
      if (signal.aborted) return;
      activeSession = pickActiveSession(body.sessions);
    } catch {
      if (!signal.aborted) activeSession = null;
    }
  }

  $effect(() => {
    const controller = new AbortController();
    void refreshNavState(controller.signal);
    const timer = setInterval(() => void refreshNavState(controller.signal), POLL_MS);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  });

  let hasProject = $derived(activeSession?.project != null);
  let sessionRunning = $derived(
    activeSession !== null && !TERMINAL_SESSION_STATUSES.has(activeSession.status),
  );
</script>

<main class="app flex h-screen flex-col overflow-hidden bg-trusty-surface text-trusty-text">
  <AppHeader />

  <div class="body flex flex-1 overflow-hidden">
    <WorkstreamRail
      collapsed={railCollapsed}
      onToggleCollapse={() => (railCollapsed = !railCollapsed)}
      {activeSession}
      onSwitchToWorkstream={() => (activeTab = 'workstream')}
    />

    <div class="actpane flex flex-1 flex-col overflow-hidden">
      <ServiceNav {activeTab} {hasProject} {isGitRepo} {sessionRunning} onSelect={(t) => (activeTab = t)} />

      <div class="actbody flex-1 overflow-y-auto p-6">
        {#if activeTab === 'workstream'}
          <WorkstreamTab />
        {:else if activeTab === 'project'}
          <ProjectTab />
        {:else if activeTab === 'agents'}
          <AgentsTab />
        {:else if activeTab === 'skills'}
          <SkillsTab />
        {:else if activeTab === 'memory'}
          <MemoryTab />
        {:else if activeTab === 'search'}
          <SearchTab />
        {:else if activeTab === 'workflow'}
          <WorkflowTab />
        {:else if activeTab === 'files'}
          <FilesTab />
        {/if}
      </div>
    </div>
  </div>

  <StatusBar />
</main>
