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
  synthesizeSkills,
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

describe('synthesizeSkills', () => {
  it('groups allow-list patterns by tool-name prefix (S-8)', () => {
    const skills = synthesizeSkills(['git_status', 'gworkspace_*', 'git_log']);
    expect(skills.map((s) => s.name)).toEqual(['git', 'gworkspace']);
    expect(skills[0].patterns).toEqual(['git_log', 'git_status']);
    expect(skills.every((s) => s.synthetic)).toBe(true);
  });

  it('keeps an unprefixed pattern as its own single-pattern skill (S-8)', () => {
    const skills = synthesizeSkills(['*']);
    expect(skills).toEqual([{ name: '*', synthetic: true, patterns: ['*'] }]);
  });

  it('drops blanks and collapses duplicates', () => {
    const skills = synthesizeSkills(['  ', 'git_log', 'git_log', ' git_status ']);
    expect(skills).toHaveLength(1);
    expect(skills[0].patterns).toEqual(['git_log', 'git_status']);
  });

  it('returns nothing for an empty allow-list', () => {
    expect(synthesizeSkills([])).toEqual([]);
  });
});

describe('KNOWLEDGE_MCP_ENDPOINTS', () => {
  it('reports the shipped disabled defaults with a reason, never as connected (C-03.2)', () => {
    const byName = Object.fromEntries(KNOWLEDGE_MCP_ENDPOINTS.map((e) => [e.name, e]));
    expect(byName['trusty-memory'].enabled).toBe(false);
    expect(byName['trusty-memory'].reason).toBeTruthy();
    expect(byName['trusty-search'].enabled).toBe(false);
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
