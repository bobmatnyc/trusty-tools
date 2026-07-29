// Unit tests for `kg.ts` (#4290 code-review finding — the `Test:` doc lines
// on this module previously cited tests that were never written; this file
// makes every one of them real).
//
// Why: Mirrors `agentConfig.test.ts`'s stubbed-`fetch` idiom — these routes
// are thin REST glue, but the null-on-404 branch and the `connected: false`
// envelope are exactly the contract `KnowledgeGraphBrowser.svelte` depends on
// to avoid rendering a degraded palace as empty data (see
// `KnowledgeGraphBrowser.test.ts` for the component-level regression against
// that same failure mode).
// What: `getKgEnvelope`'s shared 404/throw/parse contract, each fetch
// helper's query-string construction, and `connected: false` pass-through.
// Test: this file.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  fetchKgAll,
  fetchKgCount,
  fetchKgSubject,
  fetchKgSubjects,
} from './kg';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Stub `fetch` with a single canned response, capturing the request URL. */
function stubFetch(status: number, body: unknown) {
  const fn = vi.fn(async (_input: RequestInfo | URL) => ({
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  }));
  vi.stubGlobal('fetch', fn);
  return fn;
}

function requestedUrl(fn: ReturnType<typeof stubFetch>): URL {
  return new URL(String(fn.mock.calls[0][0]), 'http://localhost');
}

describe('fetchKgSubjects_returns_null_on_404', () => {
  it('returns null on 404 rather than throwing (stale roster selection)', async () => {
    stubFetch(404, { error: 'unknown agent' });
    await expect(fetchKgSubjects('ghost')).resolves.toBeNull();
  });
});

describe('fetchKgSubjects_parses_envelope', () => {
  it('passes the {palace, connected, data} envelope through verbatim', async () => {
    const payload = {
      palace: 'owner-profile',
      connected: true,
      data: [{ subject: 'bob', count: 3 }],
    };
    stubFetch(200, payload);
    const got = await fetchKgSubjects('izzie');
    expect(got?.palace).toBe('owner-profile');
    expect(got?.connected).toBe(true);
    expect(got?.data).toEqual([{ subject: 'bob', count: 3 }]);
  });

  it('surfaces connected:false with its reason and config_error, not an error throw', async () => {
    stubFetch(200, {
      palace: null,
      connected: false,
      reason: "agent's agent.toml declares no [[stores]] palace",
      config_error: 'could not parse agent.toml: missing field `model`',
      data: [],
    });
    const got = await fetchKgSubjects('broken');
    expect(got?.connected).toBe(false);
    expect(got?.reason).toContain('no [[stores]] palace');
    expect(got?.config_error).toContain('missing field');
    expect(got?.data).toEqual([]);
  });

  it('throws on a non-404 error status', async () => {
    stubFetch(500, { error: 'boom' });
    await expect(fetchKgSubjects('izzie')).rejects.toThrow(/500/);
  });
});

describe('fetchKgAll_forwards_limit_and_offset', () => {
  it('forwards limit and offset as query params', async () => {
    const fn = stubFetch(200, { palace: 'p', connected: true, data: [] });
    await fetchKgAll('izzie', 25, 50);
    const url = requestedUrl(fn);
    expect(url.pathname).toBe('/api/agents/izzie/kg/all');
    expect(url.searchParams.get('limit')).toBe('25');
    expect(url.searchParams.get('offset')).toBe('50');
  });

  it('defaults limit/offset when omitted', async () => {
    const fn = stubFetch(200, { palace: 'p', connected: true, data: [] });
    await fetchKgAll('izzie');
    const url = requestedUrl(fn);
    expect(url.searchParams.get('limit')).toBe('50');
    expect(url.searchParams.get('offset')).toBe('0');
  });
});

describe('fetchKgSubject_encodes_the_subject', () => {
  it('round-trips a subject containing reserved query characters', async () => {
    const fn = stubFetch(200, { palace: 'p', connected: true, data: [] });
    const subject = 'bob smith/likes cats & dogs?';
    await fetchKgSubject('izzie', subject);
    const url = requestedUrl(fn);
    expect(url.pathname).toBe('/api/agents/izzie/kg');
    expect(url.searchParams.get('subject')).toBe(subject);
  });

  it('percent-encodes the agent name in the path segment', async () => {
    const fn = stubFetch(200, { palace: 'p', connected: true, data: [] });
    await fetchKgSubject('agent/with slash', 'bob');
    const url = requestedUrl(fn);
    expect(url.pathname).toBe(`/api/agents/${encodeURIComponent('agent/with slash')}/kg`);
  });

  it('throws on 400 (missing subject) rather than returning null', async () => {
    stubFetch(400, { error: 'subject is required' });
    await expect(fetchKgSubject('izzie', '')).rejects.toThrow(/400/);
  });
});

describe('fetchKgCount_parses_active_count', () => {
  it('parses the {active: N} data object', async () => {
    stubFetch(200, { palace: 'p', connected: true, data: { active: 42 } });
    const got = await fetchKgCount('izzie');
    expect(got?.data.active).toBe(42);
  });

  it('surfaces connected:false alongside a zeroed active count', async () => {
    stubFetch(200, {
      palace: 'p',
      connected: false,
      reason: 'trusty-memory is unreachable',
      data: { active: 0 },
    });
    const got = await fetchKgCount('izzie');
    expect(got?.connected).toBe(false);
    expect(got?.reason).toBe('trusty-memory is unreachable');
  });

  it('returns null on 404', async () => {
    stubFetch(404, { error: 'unknown agent' });
    await expect(fetchKgCount('ghost')).resolves.toBeNull();
  });
});
