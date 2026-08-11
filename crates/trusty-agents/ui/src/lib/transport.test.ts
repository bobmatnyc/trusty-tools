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
import {
  FAST_POLL_MS,
  SLOW_POLL_MS,
  SSE_RECONNECT_MAX_MS,
  SSE_RECONNECT_MIN_MS,
  connectEventSource,
  invoke,
  listenEvent,
  mintEventTicket,
  pollIntervalMs,
  sseReconnectDelayMs,
} from './transport';

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

// #5052: the `/api/events` SSE stream is authenticated by a short-lived ticket
// minted at `POST /api/events/ticket`, because `EventSource` cannot send an
// `Authorization` header. Before the fix the stream was exempt from auth
// outright and any page in the user's browser could read live conversation
// content off `127.0.0.1`. These cases pin the client half: the mint call, the
// ticket reaching the EventSource URL, and the re-mint-on-error reconnect (the
// browser's own retry would re-use an expired ticket forever).

describe('event-stream ticket auth (#5052)', () => {
  it('mints a ticket and returns null rather than throwing on failure', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ ticket: 'tkt-1' }), { status: 200 })),
    );
    await expect(mintEventTicket()).resolves.toBe('tkt-1');

    vi.stubGlobal('fetch', vi.fn(async () => new Response('nope', { status: 401 })));
    await expect(mintEventTicket()).resolves.toBeNull();

    vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 200 })));
    await expect(mintEventTicket()).resolves.toBeNull();

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('server down');
      }),
    );
    await expect(mintEventTicket()).resolves.toBeNull();

    vi.unstubAllGlobals();
  });

  it('mints via POST and puts the ticket on the EventSource URL', async () => {
    const calls: Array<{ url: string; method?: string }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: unknown, init?: RequestInit) => {
        calls.push({ url: String(url), method: init?.method });
        return new Response(JSON.stringify({ ticket: 'tkt-2', expires_in_secs: 300 }), {
          status: 200,
        });
      }),
    );
    const opened: string[] = [];
    vi.stubGlobal(
      'EventSource',
      class {
        constructor(url: string) {
          opened.push(url);
        }
        addEventListener() {}
        close() {}
        onerror: ((e: Event) => void) | null = null;
      },
    );

    const handle = await connectEventSource('sess-1');

    expect(calls).toEqual([{ url: '/api/events/ticket', method: 'POST' }]);
    expect(opened).toHaveLength(1);
    const params = new URL(opened[0], 'http://localhost').searchParams;
    expect(params.get('ticket')).toBe('tkt-2');
    expect(params.get('session_id')).toBe('sess-1');

    handle.close();
    vi.unstubAllGlobals();
  });

  it('re-mints a fresh ticket on a transport error instead of retrying the old one', async () => {
    vi.useFakeTimers();
    let minted = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        minted += 1;
        return new Response(JSON.stringify({ ticket: `tkt-${minted}` }), { status: 200 });
      }),
    );
    const opened: string[] = [];
    let live: { onerror: ((e: Event) => void) | null } | null = null;
    vi.stubGlobal(
      'EventSource',
      class {
        onerror: ((e: Event) => void) | null = null;
        constructor(url: string) {
          opened.push(url);
          live = this;
        }
        addEventListener() {}
        close() {}
      },
    );

    const handle = await connectEventSource();
    expect(opened).toHaveLength(1);

    // Transport failure: the handle must close the socket and re-mint.
    live!.onerror?.(new Event('error'));
    await vi.advanceTimersByTimeAsync(SSE_RECONNECT_MIN_MS);
    await vi.waitFor(() => expect(opened).toHaveLength(2));
    expect(opened[1]).toContain('ticket=tkt-2');
    expect(opened[1]).not.toContain('ticket=tkt-1');

    handle.close();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('backs off exponentially and caps the reconnect delay', () => {
    expect(sseReconnectDelayMs(0)).toBe(SSE_RECONNECT_MIN_MS);
    expect(sseReconnectDelayMs(1)).toBe(2 * SSE_RECONNECT_MIN_MS);
    expect(sseReconnectDelayMs(2)).toBe(4 * SSE_RECONNECT_MIN_MS);
    expect(sseReconnectDelayMs(50)).toBe(SSE_RECONNECT_MAX_MS);
    expect(sseReconnectDelayMs(-1)).toBe(SSE_RECONNECT_MIN_MS);
  });
});
