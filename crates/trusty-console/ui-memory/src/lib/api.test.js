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

import { api, unwrapPalaceList } from './api.js';

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
