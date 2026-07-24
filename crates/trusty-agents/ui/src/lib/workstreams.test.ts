// Unit tests for the pure workstream→agent grouping heuristic (#3819).
// The fetch wrappers are thin REST glue with a fail-soft (`[]` on error)
// contract with no branching logic worth a dedicated unit test.

import { describe, it, expect } from 'vitest';
import { groupByAgent, type WorkstreamSummary } from './workstreams';

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
