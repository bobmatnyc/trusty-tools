// Store-level tests for `streamDeltaIntoTask` — the function `ChatView`
// actually calls on every `task-delta` frame. The pure attribution core is
// covered in `lib/chatStream.test.ts`; these exercise the real `messages`
// `writable<Map>` end to end, including the two misattribution races the
// PR #3763 code-critic reproduced (retask race + cross-project write) and the
// "no-op ⇒ no notify" suppression that a same-reference return alone does NOT
// give you on an object-valued Svelte store.

import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
  messages,
  addMessage,
  replaceMessageTaskId,
  streamDeltaIntoTask,
  activeProjectId,
  type Message,
} from './app';

function asst(id: string, taskId: string, content = '', speaker = 'Assistant'): Message {
  return { id, role: 'assistant', content, timestamp: 0, taskId, speaker };
}

function contentOf(projectId: string, msgId: string): string | undefined {
  return get(messages)
    .get(projectId)
    ?.find((m) => m.id === msgId)?.content;
}

beforeEach(() => {
  messages.set(new Map());
  activeProjectId.set('ctrl');
});

describe('streamDeltaIntoTask', () => {
  it('grows the matching bubble in place, preserving its id, without adding messages', () => {
    addMessage('ctrl', { id: 'user-1', role: 'user', content: 'hi', timestamp: 0 });
    addMessage('ctrl', asst('asst-1', 'real-1', ''));

    streamDeltaIntoTask('real-1', 'Hel', 'Izzie');
    streamDeltaIntoTask('real-1', 'Hello', 'Izzie');

    const list = get(messages).get('ctrl')!;
    expect(list.length).toBe(2); // no bubble spawned
    const bubble = list.find((m) => m.id === 'asst-1')!;
    expect(bubble.id).toBe('asst-1'); // stable node ⇒ no keyed-each re-mount
    expect(bubble.content).toBe('Hello');
    expect(bubble.speaker).toBe('Izzie');
  });

  it('retask race: a superseded task’s late delta lands in ITS OWN bubble, never the new one', () => {
    // Two turns in the same conversation. Turn 1 reconciled to real-1; turn 2
    // is the newer placeholder (`pending-2`). A late delta for real-1 must fill
    // turn 1 — NOT be steered by position into the newest bubble.
    addMessage('ctrl', asst('asst-1', 'real-1', 'first '));
    addMessage('ctrl', asst('asst-2', 'pending-2', ''));

    streamDeltaIntoTask('real-1', 'first answer', 'Izzie');

    expect(contentOf('ctrl', 'asst-1')).toBe('first answer'); // correct bubble
    expect(contentOf('ctrl', 'asst-2')).toBe(''); // new placeholder untouched
    expect(get(messages).get('ctrl')!.find((m) => m.id === 'asst-2')!.taskId).toBe('pending-2');
  });

  it('drops a delta whose id matches nothing yet (pre-reconcile), corrupting no bubble', () => {
    addMessage('ctrl', asst('asst-2', 'pending-2', ''));
    streamDeltaIntoTask('real-unknown', 'leak?', 'Izzie');
    expect(contentOf('ctrl', 'asst-2')).toBe(''); // still the placeholder, untouched
  });

  it('cross-project: a delta for project A fills A even while project B is viewed', () => {
    addMessage('proj-a', asst('asst-a', 'real-a', ''));
    addMessage('proj-b', asst('asst-b', 'real-b', 'B-content'));
    activeProjectId.set('proj-b'); // user is looking at project B

    streamDeltaIntoTask('real-a', 'A-answer', 'Izzie');

    expect(contentOf('proj-a', 'asst-a')).toBe('A-answer'); // routed to owner
    expect(contentOf('proj-b', 'asst-b')).toBe('B-content'); // viewed project intact
  });

  it('a no-op frame performs NO store set (subscribers are not notified)', () => {
    addMessage('ctrl', asst('asst-1', 'real-1', 'Hi', 'Izzie'));

    let notifyCount = 0;
    const unsub = messages.subscribe(() => {
      notifyCount++;
    });
    notifyCount = 0; // discount the initial synchronous subscribe call

    streamDeltaIntoTask('real-1', 'Hi', 'Izzie'); // identical ⇒ no-op
    expect(notifyCount).toBe(0);

    streamDeltaIntoTask('real-1', 'Hi there', 'Izzie'); // real change ⇒ one notify
    expect(notifyCount).toBe(1);

    unsub();
  });

  it('reconcile + stream: after replaceMessageTaskId swaps pending→real, deltas fill the bubble', () => {
    // Mirrors the live flow: InputArea creates the bubble on a `pending-` id,
    // the poll loop reconciles it to the real id, then deltas match and fill.
    addMessage('ctrl', asst('asst-1', 'pending-1', ''));
    streamDeltaIntoTask('real-1', 'early', 'Izzie'); // arrives pre-reconcile ⇒ dropped
    expect(contentOf('ctrl', 'asst-1')).toBe('');

    replaceMessageTaskId('ctrl', 'pending-1', 'real-1'); // poll loop reconciles
    streamDeltaIntoTask('real-1', 'early answer', 'Izzie');
    expect(contentOf('ctrl', 'asst-1')).toBe('early answer');
  });
});
