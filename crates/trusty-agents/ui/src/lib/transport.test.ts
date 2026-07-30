// Unit tests for transport-layer pure helpers (#3745, critic MEDIUM-3) and the
// browser-fallback `send_message` error framing (#4320, code-critic MEDIUM).
//
// Why: `pollIntervalMs` drives the browser-fallback task-poll cadence; the
// 250/500/20 backoff contract (and its 19/20 boundary) must be pinned without
// standing up a live `tagent --api` sidecar. Kept in lock-step with
// `poll_interval_ms` in `ui/src-tauri/src/task_commands.rs`.
//
// The `send_message` describe block pins the #4320 fix: a `status: "error"`
// poll response must emit `task-error` (never `task-complete`) and reject —
// this is the browser/`pnpm dev` twin of the Tauri fix in `task_commands.rs`,
// and the ONLY narrative-delivery path in that mode.

import { describe, expect, it, vi } from 'vitest';
import { FAST_POLL_MS, SLOW_POLL_MS, invoke, listenEvent, pollIntervalMs } from './transport';

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

describe('send_message error framing (#4320 browser fallback)', () => {
  it('emits task-error (never task-complete) and rejects on a status: "error" poll response', async () => {
    const seen: Array<{ name: string; detail: unknown }> = [];
    const unlistenError = await listenEvent('task-error', (p) =>
      seen.push({ name: 'task-error', detail: p }),
    );
    const unlistenComplete = await listenEvent('task-complete', (p) =>
      seen.push({ name: 'task-complete', detail: p }),
    );

    const fetchMock = vi.fn(async (url: unknown, init?: RequestInit) => {
      const u = String(url);
      if (u.endsWith('/api/task') && init?.method === 'POST') {
        return new Response(JSON.stringify({ id: 'task-1', status: 'running' }), {
          status: 200,
        });
      }
      // Any subsequent call is the poll — return a raw-failure error status,
      // mirroring `PmResponse::error`'s narrative shape.
      return new Response(
        JSON.stringify({
          status: 'error',
          narrative: 'subprocess exited with status Some(1)',
        }),
        { status: 200 },
      );
    });
    vi.stubGlobal('fetch', fetchMock);

    await expect(
      invoke('send_message', { content: 'do the thing that fails' }),
    ).rejects.toThrow('subprocess exited with status Some(1)');

    expect(seen.some((e) => e.name === 'task-error')).toBe(true);
    expect(seen.some((e) => e.name === 'task-complete')).toBe(false);
    const errorEvent = seen.find((e) => e.name === 'task-error');
    expect(errorEvent?.detail).toMatchObject({
      task_id: 'task-1',
      error: 'subprocess exited with status Some(1)',
    });

    unlistenError();
    unlistenComplete();
    vi.unstubAllGlobals();
  });

  it('still emits task-complete for a non-error terminal status (e.g. success)', async () => {
    const seen: Array<{ name: string; detail: unknown }> = [];
    const unlistenError = await listenEvent('task-error', (p) =>
      seen.push({ name: 'task-error', detail: p }),
    );
    const unlistenComplete = await listenEvent('task-complete', (p) =>
      seen.push({ name: 'task-complete', detail: p }),
    );

    const fetchMock = vi.fn(async (url: unknown, init?: RequestInit) => {
      const u = String(url);
      if (u.endsWith('/api/task') && init?.method === 'POST') {
        return new Response(JSON.stringify({ id: 'task-2', status: 'running' }), {
          status: 200,
        });
      }
      return new Response(
        JSON.stringify({ status: 'success', narrative: 'All done.' }),
        { status: 200 },
      );
    });
    vi.stubGlobal('fetch', fetchMock);

    const result = await invoke('send_message', { content: 'do the thing that works' });

    expect(result).toBe('All done.');
    expect(seen.some((e) => e.name === 'task-complete')).toBe(true);
    expect(seen.some((e) => e.name === 'task-error')).toBe(false);

    unlistenError();
    unlistenComplete();
    vi.unstubAllGlobals();
  });
});
