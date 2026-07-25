// Unit tests for `agentConfig.ts` (#3819/#3816/#3864, epic #3052).
// Most fetch/patch wrappers are thin REST glue with no branching worth
// unit-testing in isolation (mirrors `stores/app.ts`'s `fetchAgentCatalog`,
// likewise doc-commented "Test: manual"). `fetchAgentStores` is the
// exception — it has a real 404→null branch the OKG Stores pane depends on
// to distinguish a stale roster selection from an agent that simply binds
// nothing, so it gets a mocked-fetch test.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { DEFINED_LISTENERS, fetchAgentStores, type AgentStores } from './agentConfig';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Stub `fetch` with a single canned response. */
function stubFetch(status: number, body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    })),
  );
}

describe('DEFINED_LISTENERS', () => {
  it('defines exactly the gmail and google-calendar listeners', () => {
    const ids = DEFINED_LISTENERS.map((l) => l.id);
    expect(ids).toEqual(['gmail', 'google-calendar']);
  });

  it('every listener declares at least one event type', () => {
    for (const listener of DEFINED_LISTENERS) {
      expect(listener.eventTypes.length).toBeGreaterThan(0);
    }
  });
});

describe('fetchAgentStores', () => {
  it('parses a live connected binding with its stats', async () => {
    const payload: AgentStores = {
      stores: [
        {
          name: 'bob-kb',
          tree: 'okg://izzie',
          index: 'bob-kb',
          palace: 'owner-profile',
          connected: true,
          chunk_count: 552,
          index_status: 'ready',
          palace_connected: true,
        },
      ],
      issues: [],
    };
    stubFetch(200, payload);
    const got = await fetchAgentStores('izzie');
    expect(got?.stores).toHaveLength(1);
    expect(got?.stores[0].connected).toBe(true);
    expect(got?.stores[0].chunk_count).toBe(552);
    expect(got?.stores[0].tree).toBe('okg://izzie');
  });

  it('surfaces a not-connected binding with the backend reason', async () => {
    stubFetch(200, {
      stores: [
        {
          name: 'ghost-kb',
          tree: 'okg://ghosty',
          index: 'ghost-kb',
          connected: false,
          reason: 'search index `ghost-kb` is not registered on the trusty-search daemon',
        },
      ],
      issues: [],
    });
    const got = await fetchAgentStores('ghosty');
    expect(got?.stores[0].connected).toBe(false);
    expect(got?.stores[0].reason).toContain('not registered');
  });

  it('returns an empty list for an agent that binds nothing', async () => {
    stubFetch(200, { stores: [], issues: [] });
    const got = await fetchAgentStores('plain');
    expect(got?.stores).toEqual([]);
  });

  it('returns null on 404 rather than throwing (stale roster selection)', async () => {
    stubFetch(404, { error: 'unknown agent' });
    await expect(fetchAgentStores('nobody')).resolves.toBeNull();
  });

  it('throws on a non-404 error status', async () => {
    stubFetch(500, { error: 'boom' });
    await expect(fetchAgentStores('izzie')).rejects.toThrow(/500/);
  });
});
