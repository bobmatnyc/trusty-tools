import { describe, it, expect } from 'vitest';
import {
  bridgeDelta,
  StreamAccumulator,
  streamAccumulator,
  fillDeltaIntoList,
} from './chatStream';
import type { AppEvent } from './transport';
import type { Message } from '../stores/app';

/** Minimal assistant-bubble factory for the fill-in-place tests. */
function asst(overrides: Partial<Message> = {}): Message {
  return {
    id: 'asst-1',
    role: 'assistant',
    content: '',
    timestamp: 0,
    taskId: 'pending-1',
    speaker: 'Assistant',
    ...overrides,
  };
}

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

  it('exposes a shared singleton instance for cross-component streaming state', () => {
    // ChatView (grows the bubble) and InputArea (gates its progress reconcile)
    // must observe the SAME streaming state, so the export is one instance.
    expect(streamAccumulator).toBeInstanceOf(StreamAccumulator);
    streamAccumulator.append('shared-task', 'x');
    expect(streamAccumulator.isStreaming('shared-task')).toBe(true);
    streamAccumulator.finalize('shared-task');
    expect(streamAccumulator.isStreaming('shared-task')).toBe(false);
  });
});

describe('fillDeltaIntoList', () => {
  it('adopts the pending placeholder on the first delta and fills it in place', () => {
    // The delta carries the REAL backend id while the bubble still holds its
    // client-side `pending-` id: the first token must still land in the ONE
    // existing bubble (id preserved), reconciling the id — not be dropped.
    const list: Message[] = [
      { id: 'user-1', role: 'user', content: 'hi', timestamp: 0 },
      asst({ id: 'asst-1', taskId: 'pending-1', content: '' }),
    ];
    const next = fillDeltaIntoList(list, 'real-99', 'Hel', 'Izzie');
    expect(next).not.toBe(list); // changed ⇒ new array
    expect(next.length).toBe(2); // no new bubble created
    const bubble = next[1];
    expect(bubble.id).toBe('asst-1'); // SAME node id ⇒ no keyed-each re-mount
    expect(bubble.taskId).toBe('real-99'); // id reconciled
    expect(bubble.content).toBe('Hel');
    expect(bubble.speaker).toBe('Izzie'); // speaker applied in the same write
  });

  it('grows the same bubble in place across subsequent deltas (matched by real id)', () => {
    const list: Message[] = [asst({ id: 'asst-1', taskId: 'real-99', content: 'Hel' })];
    const next = fillDeltaIntoList(list, 'real-99', 'Hello, world', 'Izzie');
    expect(next[0].id).toBe('asst-1');
    expect(next[0].content).toBe('Hello, world');
    expect(next).not.toBe(list);
  });

  it('returns the SAME array reference for a no-op write (no re-render churn)', () => {
    // An identical content+speaker frame (e.g. a duplicate/empty delta) must
    // not replace the list, or the store would fire a redundant render + a
    // forced scroll reflow per token — a source of the visible flicker.
    const list: Message[] = [asst({ id: 'asst-1', taskId: 'real-99', content: 'Hi', speaker: 'Izzie' })];
    const same = fillDeltaIntoList(list, 'real-99', 'Hi', 'Izzie');
    expect(same).toBe(list);
  });

  it('keeps the existing speaker when the delta carries none', () => {
    const list: Message[] = [asst({ id: 'asst-1', taskId: 'pending-1', speaker: 'CTO Assistant', content: '' })];
    const next = fillDeltaIntoList(list, 'real-99', 'x');
    expect(next[0].speaker).toBe('CTO Assistant');
    expect(next[0].taskId).toBe('real-99');
  });

  it('adopts the MOST RECENT pending assistant bubble, never an earlier turn', () => {
    const list: Message[] = [
      asst({ id: 'asst-old', taskId: 'pending-old', content: 'stale' }),
      { id: 'user-2', role: 'user', content: 'again', timestamp: 1 },
      asst({ id: 'asst-new', taskId: 'pending-new', content: '' }),
    ];
    const next = fillDeltaIntoList(list, 'real-99', 'live', 'Izzie');
    expect(next.find((m) => m.id === 'asst-new')?.taskId).toBe('real-99');
    expect(next.find((m) => m.id === 'asst-new')?.content).toBe('live');
    expect(next.find((m) => m.id === 'asst-old')?.taskId).toBe('pending-old'); // untouched
  });

  it('is a no-op when there is no assistant bubble to fill', () => {
    const list: Message[] = [{ id: 'user-1', role: 'user', content: 'hi', timestamp: 0 }];
    expect(fillDeltaIntoList(list, 'real-99', 'x')).toBe(list);
  });
});
