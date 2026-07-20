// Why: `lib/skill-roster.ts` carries every network call `SkillsTab.svelte`
// depends on — mirrors `agent-roster.test.ts`'s coverage shape for the
// sibling tab.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  createSkill,
  deleteSkill,
  fetchSkillRoster,
  skillApiErrorMessage,
  type SkillCatalogEntry,
} from './skill-roster';

const BASE = 'http://127.0.0.1:7882';

afterEach(() => {
  vi.unstubAllGlobals();
});

function entry(overrides: Partial<SkillCatalogEntry> = {}): SkillCatalogEntry {
  return {
    name: 'systematic-debugging',
    tier: 'bundled',
    description: 'Step-by-step debugging workflow',
    ...overrides,
  };
}

describe('fetchSkillRoster', () => {
  it('GETs /skills and returns the parsed entries', async () => {
    const skills = [entry(), entry({ name: 'my-skill', tier: 'project' })];
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ skills }) });
    vi.stubGlobal('fetch', fetchMock);

    const result = await fetchSkillRoster(BASE);
    expect(result).toEqual(skills);
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/skills`, { signal: undefined });
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
    await expect(fetchSkillRoster(BASE)).rejects.toThrow('boom');
  });
});

describe('createSkill', () => {
  it('POSTs {name, content} to /skills and returns the created entry', async () => {
    const created = entry({ name: 'my-skill', tier: 'project' });
    const fetchMock = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => created });
    vi.stubGlobal('fetch', fetchMock);

    const result = await createSkill(BASE, 'my-skill', '---\nname: my-skill\n---\n\nBody.\n');
    expect(result).toEqual(created);
    expect(fetchMock).toHaveBeenCalledWith(
      `${BASE}/skills`,
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ name: 'my-skill', content: '---\nname: my-skill\n---\n\nBody.\n' }),
      }),
    );
  });

  it('throws with the daemon error message on a 400 projectless rejection', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 400,
        json: async () => ({ error: { message: 'skills are project-scoped' } }),
      }),
    );
    await expect(createSkill(BASE, 'my-skill', 'x')).rejects.toThrow('project-scoped');
  });
});

describe('deleteSkill', () => {
  it('DELETEs /skills/{name}', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200 });
    vi.stubGlobal('fetch', fetchMock);

    await deleteSkill(BASE, 'my-skill');
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/skills/my-skill`, { method: 'DELETE' });
  });

  it('throws with the daemon error message on a 404', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 404,
        json: async () => ({ error: { message: 'no disk skill named' } }),
      }),
    );
    await expect(deleteSkill(BASE, 'totally-bogus')).rejects.toThrow('no disk skill named');
  });
});

describe('skillApiErrorMessage', () => {
  it('falls back to a bare HTTP status when the body is malformed', async () => {
    const res = { status: 502, json: async () => { throw new Error('not json'); } } as unknown as Response;
    await expect(skillApiErrorMessage(res)).resolves.toBe('HTTP 502');
  });
});
