// Why: `tabVisibility` (`nav-tabs.ts`) is the pure decision logic behind
// DOC-39 AC-4.2 ("locked, not hidden" for projectless project-scoped tabs)
// and AC-9.4 (Workflow hidden, not locked, without a git-bound project) —
// see that module's doc comment. Covering it here means `ServiceNav.svelte`
// itself needs no branching logic to test independently.
// What: Exercises every tab across the 4 (hasProject × isGitRepo)
// combinations that matter, plus the fixed `NAV_TABS` catalog shape.
// Test: this file.
import { describe, expect, it } from 'vitest';
import { NAV_TABS, tabVisibility, type TabId } from './nav-tabs';

describe('NAV_TABS catalog', () => {
  it('is the 8 canonical tabs, in order (skills added by issue #3449)', () => {
    expect(NAV_TABS.map((t) => t.id)).toEqual([
      'workstream',
      'project',
      'agents',
      'skills',
      'memory',
      'search',
      'workflow',
      'files',
    ]);
  });
});

describe('tabVisibility', () => {
  it('workstream is always visible and never locked, projectless or not', () => {
    expect(tabVisibility('workstream', false, false)).toEqual({ visible: true, locked: false });
    expect(tabVisibility('workstream', true, true)).toEqual({ visible: true, locked: false });
  });

  it('project-scoped tabs (e.g. project/agents/skills/memory/search/files) are locked, not hidden, when projectless', () => {
    const scoped: TabId[] = ['project', 'agents', 'skills', 'memory', 'search', 'files'];
    for (const tab of scoped) {
      expect(tabVisibility(tab, false, false)).toEqual({ visible: true, locked: true });
    }
  });

  it('project-scoped tabs unlock once a project is bound, regardless of git-ness', () => {
    const scoped: TabId[] = ['project', 'agents', 'skills', 'memory', 'search', 'files'];
    for (const tab of scoped) {
      expect(tabVisibility(tab, true, false)).toEqual({ visible: true, locked: false });
      expect(tabVisibility(tab, true, true)).toEqual({ visible: true, locked: false });
    }
  });

  it('workflow is HIDDEN (not locked) when projectless (AC-9.4)', () => {
    expect(tabVisibility('workflow', false, false)).toEqual({ visible: false, locked: false });
  });

  it('workflow is HIDDEN when a project is bound but it is not a git repo (AC-9.4)', () => {
    expect(tabVisibility('workflow', true, false)).toEqual({ visible: false, locked: false });
  });

  it('workflow is visible (and never locked) once a git-repo project is bound', () => {
    expect(tabVisibility('workflow', true, true)).toEqual({ visible: true, locked: false });
  });
});
