// Unit tests for the pure workstream→agent grouping heuristic (#3819) and, as
// of #7456, for `fetchWorkstreams`'s fail-soft contract — the wrapper now
// branches on the body's shape, not only on the response status.

import { afterEach, describe, it, expect, vi } from 'vitest';
import { fetchWorkstreams, groupByAgent, type WorkstreamSummary } from './workstreams';

const AGENTS = [
  { id: 'izzie', label: 'Izzie' },
  { id: 'cto-assistant', label: 'CTO Assistant' },
];

function ws(name: string, last = '2026-07-24T00:00:00Z'): WorkstreamSummary {
  return { name, last_activity: last, summary: '', has_open_claim: false, areas: [], item_count: 1 };
}

describe('groupByAgent', () => {
  it('matches a known agent by name substring', () => {
    const groups = groupByAgent([ws('feat-izzie-weather-metronorth')], AGENTS);
    expect(groups).toHaveLength(1);
    expect(groups[0].agentId).toBe('izzie');
    expect(groups[0].workstreams).toHaveLength(1);
  });

  it('falls back to Other when no agent id matches', () => {
    const groups = groupByAgent([ws('fix-base-lock-rm-rf-hint')], AGENTS);
    expect(groups).toHaveLength(1);
    expect(groups[0].agentId).toBe('other');
    expect(groups[0].agentLabel).toBe('Other');
  });

  it('omits empty groups entirely', () => {
    const groups = groupByAgent([ws('feat-izzie-thing')], AGENTS);
    expect(groups.map((g) => g.agentId)).toEqual(['izzie']);
  });

  it('preserves workstream order within a group', () => {
    const a = ws('feat-izzie-a');
    const b = ws('feat-izzie-b');
    const groups = groupByAgent([a, b], AGENTS);
    expect(groups[0].workstreams.map((w) => w.name)).toEqual(['feat-izzie-a', 'feat-izzie-b']);
  });

  it('orders groups by knownAgents order, Other last', () => {
    const groups = groupByAgent(
      [ws('other-thing'), ws('feat-cto-assistant-x'), ws('feat-izzie-y')],
      AGENTS,
    );
    expect(groups.map((g) => g.agentId)).toEqual(['izzie', 'cto-assistant', 'other']);
  });
});

// #7456: a `{}` body from `GET /api/workstreams` reached `groupByAgent` through
// an unchecked cast and threw "is not iterable" out of the sidebar's mount,
// contradicting this wrapper's own "never surface a network error as a crash"
// contract.
describe('fetchWorkstreams body-shape fail-soft (#7456)', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('returns the array a well-formed 200 carries', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, json: async () => [ws('feat-izzie-a')] })),
    );
    expect((await fetchWorkstreams()).map((w) => w.name)).toEqual(['feat-izzie-a']);
  });

  it('returns [] when a 200 carries a non-array body', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, json: async () => ({}) })));
    const rows = await fetchWorkstreams();
    expect(rows).toEqual([]);
    // The crash was downstream of the cast, so assert the value is groupable.
    expect(groupByAgent(rows, AGENTS)).toEqual([]);
  });

  it('returns [] on a non-2xx response', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false, status: 503, json: async () => ({}) })));
    expect(await fetchWorkstreams()).toEqual([]);
  });
});
