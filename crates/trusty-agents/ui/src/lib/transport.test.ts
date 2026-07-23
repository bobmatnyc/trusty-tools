// Unit tests for transport-layer pure helpers (#3745, critic MEDIUM-3).
//
// Why: `pollIntervalMs` drives the browser-fallback task-poll cadence; the
// 250/500/20 backoff contract (and its 19/20 boundary) must be pinned without
// standing up a live `tagent --api` sidecar. Kept in lock-step with
// `poll_interval_ms` in `ui/src-tauri/src/task_commands.rs`.

import { describe, expect, it } from 'vitest';
import { FAST_POLL_MS, SLOW_POLL_MS, pollIntervalMs } from './transport';

describe('pollIntervalMs (#3745)', () => {
  it('uses 250ms for the first ~20 polls, then 500ms — incl. the 19/20 boundary', () => {
    const cases: Array<[number, number]> = [
      [0, FAST_POLL_MS],
      [1, FAST_POLL_MS],
      [19, FAST_POLL_MS], // last fast poll
      [20, SLOW_POLL_MS], // first slow poll (boundary)
      [21, SLOW_POLL_MS],
      [1000, SLOW_POLL_MS],
    ];
    for (const [polls, expected] of cases) {
      expect(pollIntervalMs(polls)).toBe(expected);
    }
  });

  it('exposes the documented constants', () => {
    expect(FAST_POLL_MS).toBe(250);
    expect(SLOW_POLL_MS).toBe(500);
  });
});
