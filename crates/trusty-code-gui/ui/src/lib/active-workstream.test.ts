// Why: `active-workstream.svelte.ts` (code-critic PR #3460 review, HIGH 2)
// is the shared daemon's-active-workstream store both pollers write and
// `StartWorkingForm` watches — its setter and, more importantly, its
// shared resolution rule (real pointer first, pending fallback only when
// it names a listed workstream) need pinning independently of any
// component mount, mirroring `selected-project.test.ts`'s shape for the
// sibling store.
// What: exercises the store via its public setter and the pure
// `resolveActiveWorkstreamId` helper; resets both this store and the
// pending-workstream marker between tests (module-level state persists
// across tests in the same file otherwise).
// Test: this file.
import { beforeEach, describe, expect, it } from 'vitest';
import {
  activeWorkstreamState,
  resolveActiveWorkstreamId,
  setActiveWorkstreamId,
} from './active-workstream.svelte';
import { clearPendingWorkstream, setPendingWorkstream } from './pending-workstream.svelte';

beforeEach(() => {
  setActiveWorkstreamId(null);
  clearPendingWorkstream();
});

describe('activeWorkstreamState / setActiveWorkstreamId', () => {
  it('starts null and records a set id', () => {
    expect(activeWorkstreamState.id).toBeNull();
    setActiveWorkstreamId('ws-1');
    expect(activeWorkstreamState.id).toBe('ws-1');
  });

  it('overwrites a previous id and clears via null', () => {
    setActiveWorkstreamId('ws-1');
    setActiveWorkstreamId('ws-2');
    expect(activeWorkstreamState.id).toBe('ws-2');
    setActiveWorkstreamId(null);
    expect(activeWorkstreamState.id).toBeNull();
  });
});

describe('resolveActiveWorkstreamId', () => {
  it('prefers the real active pointer when it names a listed workstream', () => {
    setPendingWorkstream('ws-pending', 'pending');
    const id = resolveActiveWorkstreamId({
      active_workstream_id: 'ws-real',
      workstreams: [{ id: 'ws-real' }, { id: 'ws-pending' }],
    });
    expect(id).toBe('ws-real');
  });

  it('falls back to the pending marker when no real pointer, only if the marker names a listed workstream', () => {
    setPendingWorkstream('ws-pending', 'pending');
    expect(
      resolveActiveWorkstreamId({
        active_workstream_id: null,
        workstreams: [{ id: 'ws-pending' }],
      }),
    ).toBe('ws-pending');
    // A stale marker naming a workstream absent from the list is ignored.
    expect(
      resolveActiveWorkstreamId({ active_workstream_id: null, workstreams: [] }),
    ).toBeNull();
  });

  it('returns null when neither pointer nor marker resolves', () => {
    expect(
      resolveActiveWorkstreamId({
        active_workstream_id: 'ws-vanished',
        workstreams: [{ id: 'ws-other' }],
      }),
    ).toBeNull();
  });
});
