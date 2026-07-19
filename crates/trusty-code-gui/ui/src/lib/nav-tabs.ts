// Why: DOC-39 shell rebuild (issue #3153, build brief archived at
// https://github.com/bobmatnyc/trusty-tools/issues/3153#issuecomment-5015805597)
// names 7 CANONICAL service-nav tabs (from mockups 9a/9b/10d/10e/11b) —
// Workstream · Project · Agents · Memory · Search · Workflow · Files —
// superseding mockup 8a's stale 5-tab draft. Two tabs have non-default
// visibility rules the brief calls out explicitly:
//   - AC-4.2 "locked, not hidden": every project-scoped tab still RENDERS
//     when the workstream is projectless, just disabled, so the operator
//     can see what binding unlocks.
//   - AC-9.4: Workflow is the one exception — HIDDEN (not locked) for a
//     projectless or non-git-repo workstream, since a workflow needs an
//     actual git-backed project to guard delivery against.
// This module is the pure decision logic behind those two rules, extracted
// so `ServiceNav.svelte` has nothing to unit-test around — the component
// only renders what this function returns.
//
// What: `NAV_TABS` is the fixed, ordered 7-tab catalog. `tabVisibility`
// returns whether a given tab should render at all (`visible`) and, if so,
// whether it should render disabled (`locked`).
// Test: `nav-tabs.test.ts`.

export type TabId = 'workstream' | 'project' | 'agents' | 'memory' | 'search' | 'workflow' | 'files';

export interface TabDef {
  id: TabId;
  label: string;
}

/** The 7 canonical service-nav tabs, in display order (brief: 9a/9b/10d/10e/11b). */
export const NAV_TABS: readonly TabDef[] = [
  { id: 'workstream', label: 'Workstream' },
  { id: 'project', label: 'Project' },
  { id: 'agents', label: 'Agents' },
  { id: 'memory', label: 'Memory' },
  { id: 'search', label: 'Search' },
  { id: 'workflow', label: 'Workflow' },
  { id: 'files', label: 'Files' },
];

export interface TabVisibility {
  /** Whether the tab renders in the nav at all. */
  visible: boolean;
  /** Whether a visible tab renders disabled (AC-4.2 "locked, not hidden"). */
  locked: boolean;
}

/**
 * Decide a nav tab's visibility/lock state for the current workstream.
 *
 * Why: AC-4.2 requires project-scoped tabs to stay visible-but-disabled when
 * projectless; AC-9.4 is the one tab that is hidden outright instead —
 * Workflow guards delivery against an actual git-backed project, so showing
 * it (even locked) with no project bound has nothing meaningful to preview.
 * What: `workstream` is never locked or hidden (it IS the projectless empty
 * state). `workflow` is hidden unless a project is bound AND it is a git
 * repo. Every other tab is locked (not hidden) whenever no project is bound.
 * Test: `nav-tabs.test.ts::tabVisibility`.
 */
export function tabVisibility(tab: TabId, hasProject: boolean, isGitRepo: boolean): TabVisibility {
  if (tab === 'workstream') return { visible: true, locked: false };
  if (tab === 'workflow') return { visible: hasProject && isGitRepo, locked: false };
  return { visible: true, locked: !hasProject };
}
