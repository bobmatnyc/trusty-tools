// Unit tests for the picker-catalog refetch edge-detector (agent-picker
// cold-start race, owner report 2026-07-23).
//
// Why: `App.svelte` re-drives the agent/model catalog loads when `apiReady`
// becomes true, fixing a cold-start race where the pickers fetched once (in
// `onMount`, before the sidecar was up) and never retried. The trigger must be
// EDGE-triggered — fire exactly on the false→true transition and NOT on any
// later reactive re-evaluation while `apiReady` stays true — so it can't
// hammer the sidecar. `shouldRefetchCatalogs` is the pure predicate that
// encodes that contract; pinning it here guards the fix without mounting the
// Svelte component.

import { describe, expect, it } from 'vitest';
import { shouldRefetchCatalogs } from './catalogRefetch';

describe('shouldRefetchCatalogs (owner report 2026-07-23)', () => {
  it('fires once on the false→true transition', () => {
    expect(shouldRefetchCatalogs(false, true)).toBe(true);
  });

  it('does not fire on re-evaluation while already ready (true→true)', () => {
    expect(shouldRefetchCatalogs(true, true)).toBe(false);
  });

  it('does not fire while the API is not ready', () => {
    expect(shouldRefetchCatalogs(false, false)).toBe(false);
    expect(shouldRefetchCatalogs(true, false)).toBe(false);
  });

  it('drives the App.svelte pattern to exactly one fire across a lifetime', () => {
    // Simulate the reactive statement re-running on each apiReady value the
    // component observes: false (init) → true (health ok) → true (later ticks).
    let prev = false;
    let fires = 0;
    for (const ready of [false, true, true, true]) {
      if (shouldRefetchCatalogs(prev, ready)) fires += 1;
      prev = ready;
    }
    expect(fires).toBe(1);
  });

  it('re-fires if apiReady is ever reset false→true again (reconnect-safe)', () => {
    let prev = false;
    let fires = 0;
    for (const ready of [true, false, true]) {
      if (shouldRefetchCatalogs(prev, ready)) fires += 1;
      prev = ready;
    }
    expect(fires).toBe(2);
  });
});
