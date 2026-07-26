// Regression coverage for the browser-mode SSE→web-bus bridge, and
// specifically for #3759: `session_done` must NOT put a narrative-bearing
// `task-complete` on the bus.
//
// The bug: the bridge emitted `task-complete` with a hardcoded
// `narrative: ''` on every `session_done`. That raced the poll loop in
// `transport.ts::send_message`, the emitter that carries the REAL narrative,
// and whichever landed last won in `ChatView` — blanking the reply to
// "(no narrative)". `ChatView.test.ts` pins the user-visible half (both
// orderings, against the real component); this file pins the bus-level
// contract that makes those orderings impossible to get wrong in the first
// place, plus the surrounding routing arms so the #3759 change can't quietly
// take a neighbour with it.

import { beforeEach, describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import { bridgeEventToWebBus } from './eventBridge';
import { listenEvent, type AppEvent } from './transport';
import { clearEventLog, eventLog } from './events';
import { workflowState, resetWorkflow } from '../stores/workflow';

/** Records every web-bus message the bridge emits, in order. */
function busRecorder() {
  const seen: Array<{ event: string; payload: unknown }> = [];
  const unlisten: Array<() => void> = [];
  const ready = Promise.all(
    ['task-progress', 'task-complete', 'task-error', 'task-delta'].map((name) =>
      listenEvent(name, (payload: unknown) => seen.push({ event: name, payload })).then((fn) =>
        unlisten.push(fn),
      ),
    ),
  );
  return {
    ready,
    seen,
    stop: () => unlisten.forEach((fn) => fn()),
  };
}

let bus: ReturnType<typeof busRecorder>;

beforeEach(async () => {
  bus?.stop();
  clearEventLog();
  resetWorkflow();
  bus = busRecorder();
  await bus.ready;
});

describe('bridge_session_done_emits_no_task_complete (#3759)', () => {
  it('puts nothing at all on the web bus for session_done', () => {
    bridgeEventToWebBus({ type: 'session_done', session_id: 't1', status: 'success' } as AppEvent);

    expect(bus.seen).toEqual([]);
  });

  it('never emits a task-complete carrying an empty narrative', () => {
    // The precise defect: an empty-narrative `task-complete` is the payload
    // ChatView renders as "(no narrative)". No bridge arm may produce one.
    bridgeEventToWebBus({ type: 'session_done', session_id: 't1', status: 'success' } as AppEvent);
    bridgeEventToWebBus({ type: 'session_done', session_id: 't1', status: 'failed' } as AppEvent);
    bridgeEventToWebBus({ type: 'session_done' } as AppEvent);

    expect(bus.seen.filter((m) => m.event === 'task-complete')).toEqual([]);
  });

  it('does not fall through to the unknown-event task-progress arm', () => {
    // The explicit `break` is load-bearing: dropping the arm entirely would
    // route `session_done` into `default:` and emit `[session_done]` progress.
    bridgeEventToWebBus({ type: 'session_done', session_id: 't1', status: 'success' } as AppEvent);

    expect(bus.seen.filter((m) => m.event === 'task-progress')).toEqual([]);
  });

  it('still does its status bookkeeping — workflow status and the Events log', () => {
    // Staying off the web bus must not make `session_done` invisible: the two
    // sinks that legitimately consume it run above the switch, unconditionally.
    bridgeEventToWebBus({ type: 'session_started', session_id: 't1' } as AppEvent);
    bridgeEventToWebBus({ type: 'session_done', session_id: 't1', status: 'success' } as AppEvent);

    expect(get(workflowState).status).toBe('done');
    expect(get(eventLog).map((r) => r.type)).toEqual(['session_started', 'session_done']);
  });

  it('maps a failed session to the failed workflow status', () => {
    bridgeEventToWebBus({ type: 'session_started', session_id: 't1' } as AppEvent);
    bridgeEventToWebBus({ type: 'session_done', session_id: 't1', status: 'failed' } as AppEvent);

    expect(get(workflowState).status).toBe('failed');
  });
});

describe('bridge_routes_known_events', () => {
  it('session_started announces progress', () => {
    bridgeEventToWebBus({ type: 'session_started', session_id: 't1' } as AppEvent);

    expect(bus.seen).toEqual([
      { event: 'task-progress', payload: { task_id: 't1', message: 'Task started…' } },
    ]);
  });

  it('session_cancelled still reaches the error bus', () => {
    bridgeEventToWebBus({ type: 'session_cancelled', session_id: 't1' } as AppEvent);

    expect(bus.seen).toEqual([
      { event: 'task-error', payload: { task_id: 't1', error: 'cancelled' } },
    ]);
  });

  it('agent_message_delta still reaches the streaming bus', () => {
    bridgeEventToWebBus({
      type: 'agent_message_delta',
      session_id: 't1',
      text: 'Hel',
      agent: 'izzie',
    } as AppEvent);

    expect(bus.seen).toEqual([
      { event: 'task-delta', payload: { task_id: 't1', text: 'Hel', agent: 'izzie', done: false } },
    ]);
  });

  it('unknown events still surface as progress', () => {
    bridgeEventToWebBus({ type: 'brand_new_event', session_id: 't1' } as unknown as AppEvent);

    expect(bus.seen).toEqual([
      { event: 'task-progress', payload: { task_id: 't1', message: '[brand_new_event]' } },
    ]);
  });
});
