// Unit tests for the pure Stop/Retask helpers (#3063 code-critic follow-up
// on #3259). See `retask.ts` for the Why/What of each function under test.

import { describe, expect, it } from 'vitest';
import {
  buildRetaskPayload,
  isPendingTaskId,
  RETASK_HISTORY_MAX_CHARS,
  RETASK_HISTORY_MAX_TURNS,
  type HistoryTurn,
} from './retask';

describe('isPendingTaskId', () => {
  it('is true for a client-side placeholder id', () => {
    expect(isPendingTaskId('pending-1737331200000')).toBe(true);
  });

  it('is false for a real backend id', () => {
    expect(isPendingTaskId('a1b2c3d4-e5f6-7890-abcd-ef1234567890')).toBe(false);
  });

  it('is false for null', () => {
    expect(isPendingTaskId(null)).toBe(false);
  });

  it('is false for an empty string', () => {
    expect(isPendingTaskId('')).toBe(false);
  });
});

describe('buildRetaskPayload', () => {
  it('returns the bare instruction when history is empty', () => {
    expect(buildRetaskPayload([], 'do the thing')).toBe('do the thing');
  });

  it('returns the bare instruction when history has no user/assistant turns', () => {
    const history: HistoryTurn[] = [
      { role: 'system', content: 'ignored' },
      { role: 'recap', content: 'also ignored' },
      { role: 'pm', content: 'also ignored' },
    ];
    expect(buildRetaskPayload(history, 'do the thing')).toBe('do the thing');
  });

  it('folds prior user/assistant turns into a transcript with no truncation marker', () => {
    const history: HistoryTurn[] = [
      { role: 'user', content: 'fix the flaky test' },
      { role: 'assistant', content: 'looking into it' },
    ];
    const result = buildRetaskPayload(history, 'actually just skip it');

    expect(result).toContain('User: fix the flaky test');
    expect(result).toContain('Assistant: looking into it');
    expect(result).toContain('[The previous task was stopped before completion.]');
    expect(result).toContain('User: actually just skip it');
    expect(result).not.toContain('…earlier turns omitted…');
    // Transcript turns precede the new instruction.
    expect(result.indexOf('fix the flaky test')).toBeLessThan(
      result.indexOf('actually just skip it'),
    );
  });

  it('drops empty/whitespace-only turns without emitting blank lines', () => {
    const history: HistoryTurn[] = [
      { role: 'user', content: '   ' },
      { role: 'assistant', content: '' },
      { role: 'user', content: 'real turn' },
    ];
    const result = buildRetaskPayload(history, 'go');
    expect(result).toBe(
      'User: real turn\n\n[The previous task was stopped before completion.]\nUser: go',
    );
  });

  it('never duplicates the new instruction into the transcript', () => {
    const history: HistoryTurn[] = [
      { role: 'user', content: 'same text as the retask' },
      { role: 'assistant', content: 'ack' },
    ];
    const result = buildRetaskPayload(history, 'same text as the retask');

    // Appears once in the transcript (as the prior user turn) and once at
    // the very end (as the new instruction) — never a spurious third copy,
    // and callers are expected to pass `history` captured BEFORE the new
    // turn is added to the store, so this isn't a "the same message counted
    // twice from history" bug either.
    const occurrences = result.split('same text as the retask').length - 1;
    expect(occurrences).toBe(2);
    expect(result.endsWith('User: same text as the retask')).toBe(true);
  });

  it('caps to the last RETASK_HISTORY_MAX_TURNS turns and adds the omitted marker', () => {
    const history: HistoryTurn[] = Array.from({ length: RETASK_HISTORY_MAX_TURNS + 5 }, (_, i) => ({
      role: i % 2 === 0 ? 'user' : 'assistant',
      content: `turn ${i}`,
    }));
    const result = buildRetaskPayload(history, 'new instruction');

    expect(result).toContain('[…earlier turns omitted…]');
    // The earliest 5 turns should have been dropped.
    expect(result).not.toContain('turn 0\n');
    expect(result).not.toContain('turn 4\n');
    // The most recent turn (index MAX_TURNS + 4) should survive.
    expect(result).toContain(`turn ${RETASK_HISTORY_MAX_TURNS + 4}`);
  });

  it('caps to the RETASK_HISTORY_MAX_CHARS char budget and adds the omitted marker', () => {
    // 15 turns stays under RETASK_HISTORY_MAX_TURNS (20), so the turn-count
    // cap never engages — isolating this test to the char-budget path. Each
    // ~2KB turn is well under the per-turn limit individually, but 15 of
    // them (~30KB joined) blows the 24KB budget.
    const bigTurn = 'x'.repeat(2000);
    const history: HistoryTurn[] = Array.from({ length: 15 }, (_, i) => ({
      role: 'user' as const,
      content: `${bigTurn}-${i}`,
    }));
    const result = buildRetaskPayload(history, 'new instruction');

    expect(result).toContain('[…earlier turns omitted…]');
    // The earliest turn should have been dropped to stay within budget.
    expect(result).not.toContain(`${bigTurn}-0`);
    // The most recent turn should survive.
    expect(result).toContain(`${bigTurn}-14`);
    // Sanity: the transcript portion (excluding the new-instruction suffix)
    // stays within budget.
    const transcriptEnd = result.indexOf('\n\n[The previous task was stopped');
    expect(transcriptEnd).toBeGreaterThan(-1);
    expect(transcriptEnd).toBeLessThanOrEqual(RETASK_HISTORY_MAX_CHARS + '[…earlier turns omitted…]\n'.length);
  });

  it('hard-truncates a single turn that alone exceeds the char budget', () => {
    const history: HistoryTurn[] = [
      { role: 'user', content: 'y'.repeat(RETASK_HISTORY_MAX_CHARS + 1000) },
    ];
    const result = buildRetaskPayload(history, 'new instruction');

    expect(result).toContain('[…earlier turns omitted…]');
    expect(result).toContain('…[truncated]');
  });
});
