// Why: `pending-workstream.svelte.ts` is the code-critic PR #3392 MEDIUM
// fix's cross-module store — `setPendingWorkstream`/`clearPendingWorkstream`
// are the only two write paths, worth pinning directly since a bug here
// would either strand the fallback set forever (masking a later, genuinely
// active workstream is impossible per the module's own docs, but a stale id
// pointing nowhere would just be dead weight) or clear it too eagerly
// (defeating the whole fix — the fallback would vanish exactly when the
// MEDIUM finding needs it to still be there).
// What: Each test resets `pendingWorkstream` to its initial `{id: null,
// name: null}` shape first (module-level state persists across tests in the
// same file otherwise), then exercises one function's documented contract.
// Test: this file.
import { beforeEach, describe, expect, it } from 'vitest';
import { clearPendingWorkstream, pendingWorkstream, setPendingWorkstream } from './pending-workstream.svelte';

beforeEach(() => {
  clearPendingWorkstream();
});

describe('setPendingWorkstream', () => {
  it('sets both id and name together', () => {
    setPendingWorkstream('ws-123', 'acme-api — 2026-07-20');
    expect(pendingWorkstream.id).toBe('ws-123');
    expect(pendingWorkstream.name).toBe('acme-api — 2026-07-20');
  });

  it('overwrites a previously-set pending workstream', () => {
    setPendingWorkstream('ws-old', 'old workstream');
    setPendingWorkstream('ws-new', 'new workstream');
    expect(pendingWorkstream.id).toBe('ws-new');
    expect(pendingWorkstream.name).toBe('new workstream');
  });
});

describe('clearPendingWorkstream', () => {
  it('resets both id and name to null', () => {
    setPendingWorkstream('ws-123', 'acme-api — 2026-07-20');
    clearPendingWorkstream();
    expect(pendingWorkstream.id).toBeNull();
    expect(pendingWorkstream.name).toBeNull();
  });

  it('is a no-op (does not throw) when already clear', () => {
    expect(() => clearPendingWorkstream()).not.toThrow();
    expect(pendingWorkstream.id).toBeNull();
  });
});
