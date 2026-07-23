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
import {
  applyDelta,
  formatElapsed,
  selectChatEntries,
  transcriptFilename,
  type AgentMessageDeltaEvent,
  type StreamBubble,
  type TurnRecord,
} from './transcript';

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

describe('transcriptFilename', () => {
  // 2026-07-18T14:05:09 local — filename stamps in LOCAL time, so build the
  // instant from local components to keep the assertion timezone-independent.
  const generatedAtMs = new Date(2026, 6, 18, 14, 5, 9).getTime();

  it('builds transcript-<id>-<YYYYMMDD-HHMMSS>.md from the export instant', () => {
    expect(transcriptFilename('ws-123', generatedAtMs)).toBe('transcript-ws-123-20260718-140509.md');
  });

  it('sanitizes unsafe characters in the workstream id', () => {
    expect(transcriptFilename('a/b c:d', generatedAtMs)).toBe(
      'transcript-a-b-c-d-20260718-140509.md',
    );
  });

  it('falls back to "workstream" when the id sanitizes to empty', () => {
    expect(transcriptFilename('///', generatedAtMs)).toBe(
      'transcript-workstream-20260718-140509.md',
    );
  });
});

describe('applyDelta (tcode streaming epic #3696, Slice 3)', () => {
  function delta(overrides: Partial<AgentMessageDeltaEvent> = {}): AgentMessageDeltaEvent {
    return {
      session_id: 'sess-1',
      agent: 'python-engineer',
      agent_id: 'agent-1',
      turn_id: 'turn-1',
      delta: '',
      done: false,
      seq: 1,
      ...overrides,
    };
  }

  it('starts from an empty bubble list with one bubble on the first delta', () => {
    const bubbles = applyDelta([], delta({ delta: 'Hel', seq: 1 }));
    expect(bubbles).toEqual([
      { agentId: 'agent-1', turnId: 'turn-1', agent: 'python-engineer', text: 'Hel', done: false, seq: 1 },
    ]);
  });

  it('two deltas of one (agent_id, turn_id) build one bubble by concatenation', () => {
    let bubbles: StreamBubble[] = [];
    bubbles = applyDelta(bubbles, delta({ delta: 'Hel', seq: 1 }));
    bubbles = applyDelta(bubbles, delta({ delta: 'lo', seq: 2 }));
    expect(bubbles).toHaveLength(1);
    expect(bubbles[0].text).toBe('Hello');
    expect(bubbles[0].done).toBe(false);
  });

  it('done:true freezes the bubble (no further growth expected, done stays true)', () => {
    let bubbles: StreamBubble[] = [];
    bubbles = applyDelta(bubbles, delta({ delta: 'Hello', seq: 1 }));
    bubbles = applyDelta(bubbles, delta({ delta: '', done: true, seq: 2 }));
    expect(bubbles[0]).toMatchObject({ text: 'Hello', done: true });

    // A later delta for the SAME key still concatenates (defensive against a
    // malformed producer), but `done` never flips back to false.
    bubbles = applyDelta(bubbles, delta({ delta: ' world', done: false, seq: 3 }));
    expect(bubbles[0]).toMatchObject({ text: 'Hello world', done: true });
  });

  it('two different (agent_id, turn_id) keys produce two bubbles ordered by seq', () => {
    let bubbles: StreamBubble[] = [];
    bubbles = applyDelta(bubbles, delta({ agent_id: 'agent-1', turn_id: 'turn-1', delta: 'first', seq: 1 }));
    bubbles = applyDelta(bubbles, delta({ agent_id: 'agent-2', turn_id: 'turn-2', delta: 'second', seq: 2 }));
    expect(bubbles).toHaveLength(2);
    expect(bubbles.map((b) => b.text)).toEqual(['first', 'second']);
  });

  it('two different agent_ids sharing the SAME turn_id value produce TWO separate bubbles (proves tuple-keying)', () => {
    let bubbles: StreamBubble[] = [];
    bubbles = applyDelta(
      bubbles,
      delta({ agent_id: 'agent-a', turn_id: 'shared-turn', agent: 'python-engineer', delta: 'from A', seq: 1 }),
    );
    bubbles = applyDelta(
      bubbles,
      delta({ agent_id: 'agent-b', turn_id: 'shared-turn', agent: 'rust-engineer', delta: 'from B', seq: 2 }),
    );
    expect(bubbles).toHaveLength(2);
    expect(bubbles.find((b) => b.agentId === 'agent-a')?.text).toBe('from A');
    expect(bubbles.find((b) => b.agentId === 'agent-b')?.text).toBe('from B');
  });

  it('orders bubbles by seq even when deltas for a later-seq bubble arrive first', () => {
    let bubbles: StreamBubble[] = [];
    // "turn-2" bubble is seeded with the HIGHER seq first...
    bubbles = applyDelta(bubbles, delta({ agent_id: 'agent-2', turn_id: 'turn-2', delta: 'later', seq: 5 }));
    // ...then "turn-1" arrives with a LOWER seq — output must still be ordered ascending by seq.
    bubbles = applyDelta(bubbles, delta({ agent_id: 'agent-1', turn_id: 'turn-1', delta: 'earlier', seq: 2 }));
    expect(bubbles.map((b) => b.text)).toEqual(['earlier', 'later']);
  });
});
