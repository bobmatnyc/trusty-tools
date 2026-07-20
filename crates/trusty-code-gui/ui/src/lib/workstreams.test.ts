// Why: `lib/workstreams.ts` carries every network call and pure
// formatting/decision rule `WorkstreamSwitcher.svelte` depends on —
// covering it here means those wire-shape and error-mapping invariants
// (`409` -> `ActiveConflictError`, no auto-force-retry) are checked without
// mounting Svelte, mirroring `new-workstream.test.ts`'s split.
// What: One `describe` block per exported function; network calls are
// exercised against a stubbed global `fetch` (`vi.stubGlobal`), matching
// `App.test.ts`'s existing stubbing convention for this crate.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ActiveConflictError,
  activateWorkstream,
  closeWorkstream,
  fetchWorkstreams,
  isActivationSignal,
  renameWorkstream,
  workstreamLabel,
  workstreamStateDotClass,
  type Workstream,
  type WorkstreamListResponse,
} from './workstreams';

const BASE = 'http://127.0.0.1:7882';

afterEach(() => {
  vi.unstubAllGlobals();
});

function ws(overrides: Partial<Workstream> = {}): Workstream {
  return {
    id: 'ws-1',
    name: 'Token rotation hardening',
    state: 'idle',
    session_ids: [],
    created_at: '2026-07-19T14:32:00Z',
    updated_at: '2026-07-19T14:32:00Z',
    metadata: {},
    ...overrides,
  };
}

describe('fetchWorkstreams', () => {
  it('GETs /workstreams and returns the parsed envelope', async () => {
    const body: WorkstreamListResponse = { active_workstream_id: 'ws-1', workstreams: [ws()] };
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => body });
    vi.stubGlobal('fetch', fetchMock);

    const result = await fetchWorkstreams(BASE);
    expect(result).toEqual(body);
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/workstreams`, { signal: undefined });
  });

  it('throws on a non-ok response', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 503 }));
    await expect(fetchWorkstreams(BASE)).rejects.toThrow('HTTP 503');
  });
});

describe('activateWorkstream', () => {
  it('POSTs {force: false} to /workstreams/{id}/activate', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200 });
    vi.stubGlobal('fetch', fetchMock);

    await activateWorkstream(BASE, 'ws-1');
    expect(fetchMock).toHaveBeenCalledWith(
      `${BASE}/workstreams/ws-1/activate`,
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ force: false }),
      }),
    );
  });

  it('throws ActiveConflictError on 409 (DOC-48 §6.1), never auto-retries with force', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: false, status: 409 });
    vi.stubGlobal('fetch', fetchMock);

    await expect(activateWorkstream(BASE, 'ws-1')).rejects.toBeInstanceOf(ActiveConflictError);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('throws a plain error on any other non-ok status', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 404 }));
    await expect(activateWorkstream(BASE, 'missing')).rejects.toThrow('HTTP 404');
  });
});

describe('closeWorkstream', () => {
  it('POSTs to /workstreams/{id}/close with no body', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal('fetch', fetchMock);

    await closeWorkstream(BASE, 'ws-1');
    expect(fetchMock).toHaveBeenCalledWith(`${BASE}/workstreams/ws-1/close`, { method: 'POST' });
  });

  it('throws on a non-ok response', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 404 }));
    await expect(closeWorkstream(BASE, 'missing')).rejects.toThrow('HTTP 404');
  });
});

describe('renameWorkstream', () => {
  it('POSTs {name} and returns the updated record', async () => {
    const updated = ws({ name: 'renamed' });
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => updated });
    vi.stubGlobal('fetch', fetchMock);

    const result = await renameWorkstream(BASE, 'ws-1', 'renamed');
    expect(result).toEqual(updated);
    expect(fetchMock).toHaveBeenCalledWith(
      `${BASE}/workstreams/ws-1/rename`,
      expect.objectContaining({ method: 'POST', body: JSON.stringify({ name: 'renamed' }) }),
    );
  });

  it('throws on a non-ok response', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 404 }));
    await expect(renameWorkstream(BASE, 'missing', 'x')).rejects.toThrow('HTTP 404');
  });
});

describe('workstreamLabel', () => {
  it('returns the name when non-blank', () => {
    expect(workstreamLabel(ws({ name: 'Token rotation' }))).toBe('Token rotation');
  });

  it('falls back to a placeholder for an empty or whitespace-only name', () => {
    expect(workstreamLabel(ws({ name: '' }))).toBe('(untitled workstream)');
    expect(workstreamLabel(ws({ name: '   ' }))).toBe('(untitled workstream)');
  });
});

describe('workstreamStateDotClass', () => {
  it('maps each state to a distinct status-dot class', () => {
    expect(workstreamStateDotClass('active')).toBe('bg-status-ok');
    expect(workstreamStateDotClass('idle')).toBe('bg-status-warn');
    expect(workstreamStateDotClass('closed')).toBe('bg-status-neutral');
  });
});

describe('isActivationSignal', () => {
  it('is true for workstream_activation_changed and workstream_state_inferred', () => {
    expect(
      isActivationSignal({
        session_id: '',
        event_type: 'workstream_activation_changed',
        payload: { type: 'workstream_activation_changed', new_active_id: 'ws-2', prior_id: 'ws-1' },
      }),
    ).toBe(true);
    expect(
      isActivationSignal({
        session_id: '',
        event_type: 'workstream_state_inferred',
        payload: { type: 'workstream_state_inferred', workstream_id: 'ws-1', state: 'closed' },
      }),
    ).toBe(true);
  });

  it('is false for any other event type (e.g. a session-scoped event)', () => {
    expect(
      isActivationSignal({
        session_id: 'sess-1',
        event_type: 'session_activity_update',
        payload: { type: 'session_activity_update' },
      }),
    ).toBe(false);
  });
});
