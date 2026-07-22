// Why: `selected-project.svelte.ts` is issue #3447 bug 1's fix vehicle — the
// shared cross-module store `ProjectPickerModal`/`StartWorkingForm` and the
// rail's new Projects section all read/write. `selectProject` is the one
// write path; worth pinning directly since a bug here (e.g. reassigning the
// exported object instead of mutating its property) would silently break
// reactivity for every importer, not just one component's local test.
// What: each test resets `selectedProjectState.project` to `null` first
// (module-level state persists across tests in the same file otherwise),
// then exercises the documented contract.
// Test: this file.
import { beforeEach, describe, expect, it } from 'vitest';
import { selectedProjectState, selectProject } from './selected-project.svelte';
import type { ProjectSelection } from './new-workstream';

const PROJECT: ProjectSelection = {
  path: '/Users/bob/code/acme-api',
  displayPath: 'acme-api',
  isGitRepo: true,
};

beforeEach(() => {
  selectProject(null);
});

describe('selectProject', () => {
  it('sets the shared selection', () => {
    selectProject(PROJECT);
    expect(selectedProjectState.project).toEqual(PROJECT);
  });

  it('clears the shared selection back to projectless via null', () => {
    selectProject(PROJECT);
    selectProject(null);
    expect(selectedProjectState.project).toBeNull();
  });

  it('overwrites a previous selection with a new one', () => {
    selectProject(PROJECT);
    const other: ProjectSelection = { path: '/other', displayPath: 'other', isGitRepo: false };
    selectProject(other);
    expect(selectedProjectState.project).toEqual(other);
  });
});
