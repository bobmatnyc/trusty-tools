// Why: `selectChatEntries` and `formatElapsed` are the two pieces of
// client-side derivation `WorkstreamActivity.svelte` relies on — a wrong
// mapping or a wrong elapsed format would silently mislead the operator
// about session state, with no error surfaced anywhere. Worth pinning
// directly, independent of the component's DOM/polling concerns.
// What: covers `selectChatEntries`'s documented cases (empty turns, order
// preservation, a tool-only turn, a fully-empty turn, full-text
// preservation with no truncation — issue #3446 dropped the prior bounded/
// truncated `selectTranscriptTail`) and `formatElapsed`'s documented
// boundaries (0s, sub-minute, multi-minute, multi-hour, and the exact
// 60s/3600s rollover points).
// Test: this file.
import { describe, expect, it } from 'vitest';
import { formatElapsed, selectChatEntries, type TurnRecord } from './transcript';

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

describe('selectChatEntries', () => {
  it('returns an empty stream for an empty turns array', () => {
    expect(selectChatEntries([])).toEqual([]);
  });

  it('preserves turn order (oldest first — the wire order)', () => {
    const turns = Array.from({ length: 4 }, (_, i) =>
      turn({ role: `agent-${i}`, text: `turn ${i}` }),
    );
    const entries = selectChatEntries(turns);
    expect(entries.map((e) => e.agent)).toEqual(['agent-0', 'agent-1', 'agent-2', 'agent-3']);
  });

  it('renders a tool-only turn as "ran: toolA, toolB"', () => {
    const entries = selectChatEntries([
      turn({ role: 'python-engineer', text: '', tool_calls: ['Read', 'Edit'] }),
    ]);
    expect(entries).toEqual([{ agent: 'python-engineer', text: 'ran: Read, Edit' }]);
  });

  it('renders a fully-empty turn (no text, no tool calls) as a labeled placeholder', () => {
    const entries = selectChatEntries([turn({ role: 'pm', text: '', tool_calls: [] })]);
    expect(entries).toEqual([{ agent: 'pm', text: '(no output)' }]);
  });

  it('preserves long prose text verbatim, with no truncation (issue #3446)', () => {
    const longText = 'x'.repeat(500);
    const entries = selectChatEntries([turn({ text: longText })]);
    expect(entries[0].text).toBe(longText);
    expect(entries[0].text).toHaveLength(500);
  });

  it('leaves short prose text unchanged', () => {
    const entries = selectChatEntries([turn({ text: 'short reply' })]);
    expect(entries[0].text).toBe('short reply');
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
