// Unit tests for `agentConfig.ts` (#3819/#3816/#3864, epic #3052).
// Most fetch/patch wrappers are thin REST glue with no branching worth
// unit-testing in isolation (mirrors `stores/app.ts`'s `fetchAgentCatalog`,
// likewise doc-commented "Test: manual"). `fetchAgentStores` is the
// exception — it has a real 404→null branch the OKG Stores pane depends on
// to distinguish a stale roster selection from an agent that simply binds
// nothing, so it gets a mocked-fetch test.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  DEFINED_LISTENERS,
  KNOWLEDGE_MCP_ENDPOINTS,
  STORE_BOUND_KNOWLEDGE_TOOLS,
  fetchAgentStores,
  matchesToolGlob,
  fetchAgentSkills,
  type AgentStores,
} from './agentConfig';

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

// --- DOC-57 Phase-1 derivations (#3932) -------------------------------------

describe('matchesToolGlob', () => {
  it('matches exact names', () => {
    expect(matchesToolGlob('vector_search', 'vector_search')).toBe(true);
    expect(matchesToolGlob('vector_search', 'vector_searchx')).toBe(false);
  });

  it('matches trailing wildcard prefixes, and only trailing ones', () => {
    expect(matchesToolGlob('vector_*', 'vector_search')).toBe(true);
    expect(matchesToolGlob('*', 'anything_at_all')).toBe(true);
    // A leading wildcard is a listener-filter dialect, not an allow-list one
    // — the server's `match_any_glob` treats it as a literal, and so must this.
    expect(matchesToolGlob('*_search', 'vector_search')).toBe(false);
  });
});

describe('fetchAgentSkills', () => {
  // Why: a stale roster selection produces a 404, and that is a normal outcome
  // the pane branches on — not an exception the shell has to catch. Same
  // contract as `fetchAgentStores`.
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  it('returns null on 404 rather than throwing', async () => {
    globalThis.fetch = (async () =>
      new Response('{}', { status: 404 })) as unknown as typeof fetch;
    await expect(fetchAgentSkills('ghost')).resolves.toBeNull();
  });

  it('parses the resolved one-skill-per-tool payload', async () => {
    const body = {
      skills: [
        {
          id: 'mta-train-time',
          name: 'MTA Train Time',
          description: 'Next Metro-North departures.',
          kind: 'action',
          origin: { kind: 'builtin' },
          granted: true,
          tools: ['get_train_schedule'],
          provider: null,
        },
      ],
      granted_count: 1,
      unresolved: [],
      unmatched_patterns: [],
      dead_scope_patterns: [],
      declares_capability: true,
    };
    globalThis.fetch = (async () =>
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })) as unknown as typeof fetch;
    const got = await fetchAgentSkills('izzie');
    expect(got?.skills[0].name).toBe('MTA Train Time');
    expect(got?.skills[0].tools).toEqual(['get_train_schedule']);
  });

  it('throws on a non-404 failure so the pane can say the route is down', async () => {
    globalThis.fetch = (async () =>
      new Response('{}', { status: 503 })) as unknown as typeof fetch;
    await expect(fetchAgentSkills('izzie')).rejects.toThrow('503');
  });
});

describe('KNOWLEDGE_MCP_ENDPOINTS', () => {
  it('reports the shipped defaults, with a reason iff disabled, never fabricated (C-03.2)', () => {
    const byName = Object.fromEntries(KNOWLEDGE_MCP_ENDPOINTS.map((e) => [e.name, e]));
    // trusty-memory/trusty-search moved from dead `[[tool_registry.endpoints]]`
    // OpenRPC stubs to live `[[mcp.services]]` MCP-stdio entries — both now
    // ship enabled by default, same as gworkspace.
    expect(byName['trusty-memory'].enabled).toBe(true);
    expect(byName['trusty-memory'].reason).toBeFalsy();
    expect(byName['trusty-search'].enabled).toBe(true);
    expect(byName['trusty-search'].reason).toBeFalsy();
    expect(byName['gworkspace'].enabled).toBe(true);
    // A disabled endpoint carries a reason; an enabled one has nothing to
    // explain. The invariant, not the individual flags, is what must hold.
    for (const endpoint of KNOWLEDGE_MCP_ENDPOINTS) {
      expect(Boolean(endpoint.reason)).toBe(!endpoint.enabled);
      expect(endpoint.scopes.length).toBeGreaterThan(0);
    }
  });
});

describe('STORE_BOUND_KNOWLEDGE_TOOLS', () => {
  it('is the vector_search seam and nothing more (§4.3)', () => {
    // Widening this to §4.3's illustrative tool list would be the hardcoded
    // taxonomy that same section warns against; classification belongs to
    // `kind = "knowledge"` manifests (#3933).
    expect([...STORE_BOUND_KNOWLEDGE_TOOLS]).toEqual(['vector_search']);
  });
});
