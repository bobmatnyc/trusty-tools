import { describe, it, expect } from 'vitest';
import { bridgeDelta, StreamAccumulator } from './chatStream';
import type { AppEvent } from './transport';

describe('bridgeDelta', () => {
  it('maps an agent_message_delta event to a task-delta payload', () => {
    const ev: AppEvent = {
      type: 'agent_message_delta',
      session_id: 'task-1',
      agent: 'Izzie',
      text: 'Hel',
      done: false,
    };
    expect(bridgeDelta(ev)).toEqual({
      task_id: 'task-1',
      text: 'Hel',
      agent: 'Izzie',
      done: false,
    });
  });

  it('defaults missing fields and preserves the terminal done marker', () => {
    const ev: AppEvent = {
      type: 'agent_message_delta',
      session_id: 'task-1',
      done: true,
    };
    expect(bridgeDelta(ev)).toEqual({
      task_id: 'task-1',
      text: '',
      agent: '',
      done: true,
    });
  });

  it('returns null for any other event type', () => {
    expect(bridgeDelta({ type: 'pm_thinking', session_id: 'x', text: 'hi' })).toBeNull();
    expect(bridgeDelta({ type: 'session_done', session_id: 'x' })).toBeNull();
  });
});

describe('StreamAccumulator', () => {
  it('accumulates fragments in order and returns the running full text', () => {
    const acc = new StreamAccumulator();
    expect(acc.append('t', 'The ')).toBe('The ');
    expect(acc.append('t', 'quick ')).toBe('The quick ');
    expect(acc.append('t', 'fox')).toBe('The quick fox');
    expect(acc.get('t')).toBe('The quick fox');
  });

  it('tracks per-task streaming state independently', () => {
    const acc = new StreamAccumulator();
    expect(acc.isStreaming('a')).toBe(false);
    acc.append('a', 'x');
    expect(acc.isStreaming('a')).toBe(true);
    expect(acc.isStreaming('b')).toBe(false);
  });

  it('an empty fragment still marks the task as streaming', () => {
    const acc = new StreamAccumulator();
    acc.append('t', '');
    expect(acc.isStreaming('t')).toBe(true);
    expect(acc.get('t')).toBe('');
  });

  it('finalize forgets the buffer so the authoritative result can replace it', () => {
    const acc = new StreamAccumulator();
    acc.append('t', 'partial stream');
    expect(acc.isStreaming('t')).toBe(true);
    expect(acc.finalize('t')).toBe(true);
    // After finalize the task is no longer streaming: progress ticks and the
    // final narrative are free to overwrite (dedupe — no double render).
    expect(acc.isStreaming('t')).toBe(false);
    expect(acc.get('t')).toBeUndefined();
    // Idempotent — finalizing an unknown/already-cleared task is a no-op.
    expect(acc.finalize('t')).toBe(false);
  });

  it('models the full stream→complete dedupe lifecycle', () => {
    const acc = new StreamAccumulator();
    // Fragments accumulate.
    acc.append('t', 'Hello');
    acc.append('t', ', world');
    expect(acc.get('t')).toBe('Hello, world');
    // While streaming, progress ticks must be suppressed.
    expect(acc.isStreaming('t')).toBe(true);
    // Completion clears the buffer; the authoritative narrative then replaces
    // the bubble content (handled by the component's updateMessageByTask).
    acc.finalize('t');
    expect(acc.isStreaming('t')).toBe(false);
  });
});
