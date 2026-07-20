// Why: `lib/project-roster.ts` carries the wire-shape guard and transport
// `ProjectPickerModal.svelte` depends on for correctness — covering it here
// means those invariants are checked without mounting Svelte or touching a
// real `fetch`, mirroring `workstreams.test.ts`'s split for its own module.
// What: one `describe` block per exported function.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  fetchProjectRoster,
  isProjectRoster,
  projectCandidateLabel,
  type ProjectCandidate,
} from './project-roster';

afterEach(() => {
  vi.unstubAllGlobals();
});

const OWNED: ProjectCandidate = { name: 'trusty-tools', path: '/home/bob/x/trusty-tools', owner: 'bobmatnyc' };
const UNOWNED: ProjectCandidate = { name: 'acme-api', path: '/home/bob/acme-api', owner: null };

describe('isProjectRoster', () => {
  it('accepts a well-formed roster, including an empty entries array', () => {
    expect(isProjectRoster({ entries: [] })).toBe(true);
    expect(isProjectRoster({ entries: [OWNED, UNOWNED] })).toBe(true);
  });

  it('rejects non-object bodies (null, string, array)', () => {
    expect(isProjectRoster(null)).toBe(false);
    expect(isProjectRoster('ok')).toBe(false);
    expect(isProjectRoster([{ entries: [] }])).toBe(false);
  });

  it('rejects a body missing entries or with non-array entries', () => {
    expect(isProjectRoster({})).toBe(false);
    expect(isProjectRoster({ entries: 'oops' })).toBe(false);
  });

  it('rejects entries with drifted field shapes', () => {
    expect(isProjectRoster({ entries: [{ name: 'x' }] })).toBe(false);
    expect(isProjectRoster({ entries: [{ name: 'x', path: '/x', owner: 7 }] })).toBe(false);
  });

  it('issue #3435: accepts an entry with a boolean registered field, present or absent', () => {
    expect(
      isProjectRoster({ entries: [{ name: 'x', path: '/x', owner: null, registered: true }] }),
    ).toBe(true);
    expect(
      isProjectRoster({ entries: [{ name: 'x', path: '/x', owner: null, registered: false }] }),
    ).toBe(true);
    // Backward compatibility: an older daemon predating this field must
    // still validate.
    expect(isProjectRoster({ entries: [{ name: 'x', path: '/x', owner: null }] })).toBe(true);
  });

  it('issue #3435: rejects an entry whose registered field is present but non-boolean', () => {
    expect(
      isProjectRoster({ entries: [{ name: 'x', path: '/x', owner: null, registered: 'yes' }] }),
    ).toBe(false);
  });
});

describe('fetchProjectRoster', () => {
  it('returns the parsed roster on a 200 with a well-formed body', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ entries: [OWNED] }) }) as Response),
    );

    await expect(fetchProjectRoster('http://x')).resolves.toEqual({ entries: [OWNED] });
  });

  it('throws on a non-OK status', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: false, status: 503, json: async () => ({}) }) as Response),
    );

    await expect(fetchProjectRoster('http://x')).rejects.toThrow('HTTP 503');
  });

  it('throws on a 200 with a shape-invalid body rather than returning it (PR #3103 precedent)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ ok: true }) }) as Response),
    );

    await expect(fetchProjectRoster('http://x')).rejects.toThrow('malformed response');
  });
});

describe('projectCandidateLabel', () => {
  it('prefixes the owner when present', () => {
    expect(projectCandidateLabel(OWNED)).toBe('bobmatnyc/trusty-tools');
  });

  it('is just the name when owner is null', () => {
    expect(projectCandidateLabel(UNOWNED)).toBe('acme-api');
  });
});
