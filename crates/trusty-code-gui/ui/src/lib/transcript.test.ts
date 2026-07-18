// Why: `selectTranscriptTail` and `formatElapsed` are the two pieces of
// client-side derivation `SessionMonitor.svelte` relies on — a wrong tail
// selection or a wrong elapsed format would silently mislead the operator
// about session state, with no error surfaced anywhere. Worth pinning
// directly, independent of the component's DOM/polling concerns.
// What: covers `selectTranscriptTail`'s documented cases (empty turns, more
// turns than N, a tool-only turn, a fully-empty turn) and `formatElapsed`'s
// documented boundaries (0s, sub-minute, multi-minute, multi-hour, and the
// exact 60s/3600s rollover points).
// Test: this file.
import { describe, expect, it } from 'vitest';
import { formatElapsed, selectTranscriptTail, type TurnRecord } from './transcript';

function turn(overrides: Partial<TurnRecord> = {}): TurnRecord {
  return {
    role: 'pm',
    model: 'claude-sonnet',
    text: '',
    tool_calls: [],
    ran_test_command: false,
    usage: null,
    ...overrides,
  };
}

describe('selectTranscriptTail', () => {
  it('returns an empty tail for an empty turns array', () => {
    expect(selectTranscriptTail([])).toEqual([]);
  });

  it('returns only the trailing N turns when there are more than N', () => {
    const turns = Array.from({ length: 8 }, (_, i) =>
      turn({ role: `agent-${i}`, text: `turn ${i}` }),
    );
    const tail = selectTranscriptTail(turns, 5);
    expect(tail).toHaveLength(5);
    expect(tail.map((t) => t.agent)).toEqual([
      'agent-3',
      'agent-4',
      'agent-5',
      'agent-6',
      'agent-7',
    ]);
  });

  it('renders a tool-only turn as "ran: toolA, toolB"', () => {
    const tail = selectTranscriptTail([
      turn({ role: 'python-engineer', text: '', tool_calls: ['Read', 'Edit'] }),
    ]);
    expect(tail).toEqual([{ agent: 'python-engineer', preview: 'ran: Read, Edit' }]);
  });

  it('renders a fully-empty turn (no text, no tool calls) as a labeled placeholder', () => {
    const tail = selectTranscriptTail([turn({ role: 'pm', text: '', tool_calls: [] })]);
    expect(tail).toEqual([{ agent: 'pm', preview: '(no output)' }]);
  });

  it('truncates long prose text with an ellipsis', () => {
    const longText = 'x'.repeat(200);
    const tail = selectTranscriptTail([turn({ text: longText })]);
    expect(tail[0].preview).toHaveLength(161); // 160 chars + ellipsis
    expect(tail[0].preview.endsWith('…')).toBe(true);
  });

  it('leaves short prose text unchanged', () => {
    const tail = selectTranscriptTail([turn({ text: 'short reply' })]);
    expect(tail[0].preview).toBe('short reply');
  });
});

describe('formatElapsed', () => {
  const base = Date.parse('2026-07-18T10:00:00Z');

  it('formats 0s at zero elapsed', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base)).toBe('0s');
  });

  it('formats a sub-minute duration as seconds only', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base + 45_000)).toBe('45s');
  });

  it('formats a multi-minute duration as "Xm SSs"', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base + 4 * 60_000 + 12_000)).toBe('4m 12s');
  });

  it('formats a multi-hour duration as "Xh MMm"', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base + 60 * 60_000 + 2 * 60_000)).toBe('1h 02m');
  });

  it('rolls over exactly at 60 seconds', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base + 60_000)).toBe('1m 00s');
  });

  it('rolls over exactly at 3600 seconds', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base + 3_600_000)).toBe('1h 00m');
  });

  it('clamps a negative delta (now before created_at) to 0s', () => {
    expect(formatElapsed('2026-07-18T10:00:00Z', base - 5_000)).toBe('0s');
  });
});
