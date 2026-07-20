// Why: `lib/agent-roster.ts` carries every network call `AgentsTab.svelte`
// and `StartWorkingForm.svelte`'s agent selector depend on — covering it
// here means the wire-shape/error-mapping invariants are checked without
// mounting Svelte, mirroring `workstreams.test.ts`'s split.
// What: One `describe` block per exported function; network calls are
// exercised against a stubbed global `fetch` (`vi.stubGlobal`), matching
// `workstreams.test.ts`'s existing stubbing convention.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  agentApiErrorMessage,
  createAgent,
  deleteAgent,
  fetchAgentRoster,
  type AgentCatalogEntry,
} from './agent-roster';

const BASE = 'http://127.0.0.1:7882';

afterEach(() => {
  vi.unstubAllGlobals();
});

function entry(overrides: Partial<AgentCatalogEntry> = {}): AgentCatalogEntry {
  return {
    name: 'engineer',
    tier: 'embedded',
    description: 'Generalist implementation agent',
    model: null,
    ...overrides,
  };
}

describe('fetchAgentRoster', () => {
  it('GETs /agents and returns the parsed entries', async () => {
    const agents = [entry(), entry({ name: 'my-agent', tier: 'project' })];
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ agents }) });
    vi.stubGlobal('fetch', fetchMock);

    const result = await fetchAgentRoster(BASE);
    expect(result).toEqual(agents);
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/agents`, { signal: undefined });
  });

  it('throws with the daemon error message on a non-ok response', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 500,
        json: async () => ({ error: { message: 'boom' } }),
      }),
    );
    await expect(fetchAgentRoster(BASE)).rejects.toThrow('boom');
  });
});

describe('createAgent', () => {
  it('POSTs {name, content} to /agents and returns the created entry', async () => {
    const created = entry({ name: 'my-agent', tier: 'project' });
    const fetchMock = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => created });
    vi.stubGlobal('fetch', fetchMock);

    const result = await createAgent(BASE, 'my-agent', '---\nname: my-agent\n---\n\nBody.\n');
    expect(result).toEqual(created);
    expect(fetchMock).toHaveBeenCalledWith(
      `${BASE}/agents`,
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ name: 'my-agent', content: '---\nname: my-agent\n---\n\nBody.\n' }),
      }),
    );
  });

  it('throws with the daemon error message on a 403 embedded-name collision', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 403,
        json: async () => ({ error: { message: "'engineer' is an embedded agent" } }),
      }),
    );
    await expect(createAgent(BASE, 'engineer', 'x')).rejects.toThrow('embedded agent');
  });
});

describe('deleteAgent', () => {
  it('DELETEs /agents/{name}', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200 });
    vi.stubGlobal('fetch', fetchMock);

    await deleteAgent(BASE, 'my-agent');
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/agents/my-agent`, { method: 'DELETE' });
  });

  it('URL-encodes the name', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200 });
    vi.stubGlobal('fetch', fetchMock);

    await deleteAgent(BASE, 'weird name');
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/agents/weird%20name`, { method: 'DELETE' });
  });

  it('throws with the daemon error message on a 404', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 404,
        json: async () => ({ error: { message: 'no disk agent named' } }),
      }),
    );
    await expect(deleteAgent(BASE, 'totally-bogus')).rejects.toThrow('no disk agent named');
  });
});

describe('agentApiErrorMessage', () => {
  it('falls back to a bare HTTP status when the body is malformed', async () => {
    const res = { status: 502, json: async () => { throw new Error('not json'); } } as unknown as Response;
    await expect(agentApiErrorMessage(res)).resolves.toBe('HTTP 502');
  });
});
