// Regression tests for the palace-roster shape (issue #6155).
//
// Why: #7081 moved this SPA behind the console's `memory.*` JSON-RPC bridge.
// `GET /api/v1/palaces` used to answer a bare array of palace objects; the
// daemon method it now maps onto, `memory.palaces_list`, answers
// `{"palaces": [{id, palace, error}]}`. `Palaces.svelte` and `KG.svelte`
// iterate the result, so the wrapper reached them as
// `TypeError: S is not iterable` — `/tools/memory/#/palaces` sat on
// "Loading palaces…" forever and `#/kg` never populated its palace select.
// What: drives `api.listPalaces()` against a stubbed fetch and asserts the
// flat array the views require, including the error row the daemon emits for
// a palace whose counts could not be read.
// Test: this file — `pnpm test`.

import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApiError, api, unwrapPalaceList } from './api.js';

/** A palace row as `memory.palaces_list` serialises it. */
function row(id, palace, error = null) {
  return palace === null ? { id, error } : { id, palace, error };
}

/** Minimal `PalaceInfo` — only the fields the views read. */
function palace(id, overrides = {}) {
  return {
    id,
    name: id,
    description: null,
    drawer_count: 3,
    vector_count: 3,
    kg_triple_count: 7,
    room_count: 1,
    wing_count: 1,
    created_at: '2026-09-01T00:00:00Z',
    last_write_at: null,
    cached: true,
    ...overrides
  };
}

/** Serve one JSON body to the next `fetch`, and hand back the calls. */
function stubFetch(body) {
  const calls = [];
  vi.stubGlobal('fetch', async (url) => {
    calls.push(String(url));
    return {
      ok: true,
      status: 200,
      statusText: 'OK',
      headers: { get: () => 'application/json' },
      json: async () => body
    };
  });
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('api.listPalaces — the daemon wrapper (issue #6155)', () => {
  it('returns a flat, iterable array of palace objects', async () => {
    const calls = stubFetch({
      palaces: [row('trusty-tools', palace('trusty-tools')), row('izzie', palace('izzie'))]
    });

    const palaces = await api.listPalaces();

    expect(calls[0]).toContain('/api/v1/palaces');
    expect(Array.isArray(palaces)).toBe(true);
    // The three call sites all iterate; a wrapper object throws here.
    expect([...palaces].map((p) => p.id)).toEqual(['trusty-tools', 'izzie']);
    expect(palaces[0].drawer_count).toBe(3);
    expect(palaces[0].name).toBe('trusty-tools');
  });

  it('carries the row id onto a palace object that lacks one', () => {
    const [only] = unwrapPalaceList({
      palaces: [row('izzie', palace('izzie', { id: undefined }))]
    });

    expect(only.id).toBe('izzie');
  });

  it('keeps an unreadable palace visible, with its error and unknown counts', () => {
    const palaces = unwrapPalaceList({
      palaces: [row('broken', null, 'open failed: permission denied'), row('ok', palace('ok'))]
    });

    expect(palaces.map((p) => p.id)).toEqual(['broken', 'ok']);
    expect(palaces[0].error).toBe('open failed: permission denied');
    // `cached: false` is what `countLabel` in Palaces.svelte reads as unknown,
    // so the badges show `—` rather than a fabricated `0`.
    expect(palaces[0].cached).toBe(false);
    expect(palaces[1].error).toBeUndefined();
  });

  it('passes a bare array through, for a daemon predating the wrapper', () => {
    expect(unwrapPalaceList([palace('izzie')]).map((p) => p.id)).toEqual(['izzie']);
  });

  it('answers an empty array for a missing or malformed body', () => {
    expect(unwrapPalaceList(null)).toEqual([]);
    expect(unwrapPalaceList({})).toEqual([]);
    expect(unwrapPalaceList({ palaces: [null, 'nonsense'] })).toEqual([]);
  });
});

// The fast roster and the failure surface (issue #6155).
//
// Why: `memory.palaces_list` opens every palace to count it. On the operator's
// 93-palace install that ran 8.5-11 s and repeatedly exceeded the console
// bridge's 30 s budget, so `/tools/memory` sat on "Loading palaces…" and the KG
// palace selector stayed empty with nothing said. Two things had to change: the
// views ask for names only, and a failure arrives carrying its status so a view
// can render it and offer a Retry.
describe('api.listPalaces — the fast roster (issue #6155)', () => {
  it('asks for names only, and unwraps the peeked rows', async () => {
    const calls = stubFetch({
      palaces: [
        row('trusty-tools', palace('trusty-tools', { cached: false, drawer_count: 0 })),
        row('izzie', palace('izzie', { cached: false, drawer_count: 0 }))
      ]
    });

    const palaces = await api.listPalaces({ counts: false });

    expect(calls[0]).toContain('/api/v1/palaces?counts=false');
    // Names are what the list and the KG selector render.
    expect(palaces.map((p) => p.name)).toEqual(['trusty-tools', 'izzie']);
    // `cached: false` is `countLabel`'s "unknown" — a `—` badge, not a `0`.
    expect(palaces.every((p) => p.cached === false)).toBe(true);
  });

  it('asks for counts by default, so no other caller changes', async () => {
    const calls = stubFetch({ palaces: [row('izzie', palace('izzie'))] });

    await api.listPalaces();

    expect(calls[0]).not.toContain('counts=');
  });

  it('throws an ApiError carrying the status a view names and retries on', async () => {
    vi.stubGlobal('fetch', async () => ({
      ok: false,
      status: 502,
      statusText: 'Bad Gateway',
      headers: { get: () => 'application/json' },
      text: async () => '{"error":"the daemon did not answer in 30s"}'
    }));

    const failure = await api.listPalaces({ counts: false }).then(
      () => null,
      (e) => e
    );

    expect(failure).toBeInstanceOf(ApiError);
    expect(failure.status).toBe(502);
    expect(failure.message).toContain('502 Bad Gateway');
  });

  it('reports a transport failure as status 0 rather than a bare rejection', async () => {
    vi.stubGlobal('fetch', async () => {
      throw new TypeError('Failed to fetch');
    });

    const failure = await api.listPalaces().then(
      () => null,
      (e) => e
    );

    expect(failure).toBeInstanceOf(ApiError);
    expect(failure.status).toBe(0);
    expect(failure.statusText).toBe('Network error');
  });

  it('gives up on a request that never answers, instead of spinning', async () => {
    // The real budget is 35 s; the point is that the abort signal is wired to
    // the fetch at all, so aborting it produces the ApiError a view renders.
    vi.stubGlobal('fetch', async (_url, opts) => {
      const signal = opts?.signal;
      expect(signal).toBeDefined();
      return await new Promise((_resolve, reject) => {
        signal.addEventListener('abort', () => {
          const err = new Error('aborted');
          err.name = 'AbortError';
          reject(err);
        });
        // Nothing ever resolves this — exactly the hung request.
        queueMicrotask(() => signal.dispatchEvent(new Event('abort')));
      });
    });

    const failure = await api.listPalaces().then(
      () => null,
      (e) => e
    );

    expect(failure).toBeInstanceOf(ApiError);
    expect(failure.status).toBe(0);
    expect(failure.statusText).toBe('Timeout');
  });
});
