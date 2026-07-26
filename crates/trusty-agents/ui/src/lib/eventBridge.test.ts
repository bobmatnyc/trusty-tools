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
import { clearSlackMirror, slackMirror } from './slack-mirror';
import { workflowState, resetWorkflow } from '../stores/workflow';
import { messages } from '../stores/app';
import { recaps } from '../stores/recap';

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
  clearSlackMirror();
  resetWorkflow();
  messages.set(new Map());
  recaps.set(new Map());
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

// Every arm of the bridge's switch, pinned (#3759 code-critic MEDIUM-2). The
// #3759 move was byte-identical, so these are PINS on behavior that already
// shipped, not assertions about new logic — the point of extracting the bridge
// was that its routing table stops being untestable, and a table that covers
// only the arms one bugfix happened to touch does not deliver that.
const ROUTED: Array<{ arm: string; ev: Record<string, unknown>; emits: Array<{ event: string; payload: unknown }> }> = [
  {
    arm: 'session_started',
    ev: { type: 'session_started', session_id: 't1' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Task started…' } }],
  },
  {
    arm: 'session_cancelled',
    ev: { type: 'session_cancelled', session_id: 't1' },
    emits: [{ event: 'task-error', payload: { task_id: 't1', error: 'cancelled' } }],
  },
  {
    arm: 'pm_thinking',
    ev: { type: 'pm_thinking', session_id: 't1', text: 'weighing options' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'weighing options' } }],
  },
  {
    arm: 'pm_thinking (no text ⇒ placeholder)',
    ev: { type: 'pm_thinking', session_id: 't1' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: '(thinking)' } }],
  },
  {
    arm: 'pm_delegating',
    ev: { type: 'pm_delegating', session_id: 't1', agent: 'izzie', task_preview: 'fix the bug' },
    emits: [
      { event: 'task-progress', payload: { task_id: 't1', message: 'Delegating to izzie: fix the bug' } },
    ],
  },
  {
    arm: 'pm_delegating (no preview)',
    ev: { type: 'pm_delegating', session_id: 't1', agent: 'izzie' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Delegating to izzie: ' } }],
  },
  {
    arm: 'agent_spawned',
    ev: { type: 'agent_spawned', session_id: 't1', agent: 'izzie' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Agent izzie starting…' } }],
  },
  {
    arm: 'agent_message',
    ev: { type: 'agent_message', session_id: 't1', agent: 'izzie', text: 'on it' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: '[izzie] on it' } }],
  },
  {
    arm: 'agent_message (no text)',
    ev: { type: 'agent_message', session_id: 't1', agent: 'izzie' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: '[izzie] ' } }],
  },
  {
    arm: 'agent_message_delta',
    ev: { type: 'agent_message_delta', session_id: 't1', text: 'Hel', agent: 'izzie' },
    emits: [
      { event: 'task-delta', payload: { task_id: 't1', text: 'Hel', agent: 'izzie', done: false } },
    ],
  },
  {
    arm: 'agent_done',
    ev: { type: 'agent_done', session_id: 't1', agent: 'izzie', status: 'success' },
    emits: [
      { event: 'task-progress', payload: { task_id: 't1', message: 'Agent izzie done (success)' } },
    ],
  },
  {
    arm: 'agent_done (no status ⇒ ok)',
    ev: { type: 'agent_done', session_id: 't1', agent: 'izzie' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Agent izzie done (ok)' } }],
  },
  {
    arm: 'agent_failed',
    ev: { type: 'agent_failed', session_id: 't1', agent: 'izzie', error: 'boom' },
    emits: [{ event: 'task-error', payload: { task_id: 't1', error: 'Agent izzie failed: boom' } }],
  },
  {
    arm: 'tool_called',
    ev: { type: 'tool_called', session_id: 't1', tool: 'grep', preview: 'foo' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Tool grep: foo' } }],
  },
  {
    arm: 'phase_started',
    ev: { type: 'phase_started', session_id: 't1', phase: 'IMPLEMENT' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Phase: IMPLEMENT' } }],
  },
  {
    arm: 'phase_done',
    ev: { type: 'phase_done', session_id: 't1', phase: 'IMPLEMENT', status: 'failed' },
    emits: [
      { event: 'task-progress', payload: { task_id: 't1', message: 'Phase IMPLEMENT failed' } },
    ],
  },
  {
    arm: 'phase_done (no status ⇒ done)',
    ev: { type: 'phase_done', session_id: 't1', phase: 'VERIFY' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: 'Phase VERIFY done' } }],
  },
  {
    arm: 'unknown event',
    ev: { type: 'brand_new_event', session_id: 't1' },
    emits: [{ event: 'task-progress', payload: { task_id: 't1', message: '[brand_new_event]' } }],
  },
  // The four arms that deliberately stay OFF the task web bus. `session_done`
  // is #3759's own arm and is asserted in depth above; these three were always
  // silent and must stay that way — a `default:` fall-through would spam every
  // one of them onto `task-progress`.
  { arm: 'recap_generated', ev: { type: 'recap_generated', session_id: 't1', summary: 's' }, emits: [] },
  { arm: 'slack_message_received', ev: { type: 'slack_message_received', channel: 'C1', text: 'hi' }, emits: [] },
  { arm: 'slack_reply_sent', ev: { type: 'slack_reply_sent', channel: 'C1', text: 'yo' }, emits: [] },
  { arm: 'ping', ev: { type: 'ping' }, emits: [] },
  { arm: 'lag', ev: { type: 'lag' }, emits: [] },
];

describe('bridge_routes_known_events', () => {
  it.each(ROUTED)('$arm', ({ ev, emits }) => {
    bridgeEventToWebBus(ev as unknown as AppEvent);

    expect(bus.seen).toEqual(emits);
  });
});

describe('bridge_fans_out_to_non_bus_sinks', () => {
  it('recap_generated fills the recap store and the chat scrollback', () => {
    bridgeEventToWebBus({
      type: 'recap_generated',
      session_id: 't1',
      summary: 'Shipped the fix',
      table_rows: [
        ['build', 'ok'],
        ['malformed row'],
      ],
    } as unknown as AppEvent);

    const recap = get(recaps).get('t1');
    expect(recap?.summary).toBe('Shipped the fix');
    // Rows that aren't 2-tuples are dropped rather than rendered half-empty.
    expect(recap?.table_rows).toEqual([['build', 'ok']]);

    const banner = get(messages).get('t1')?.[0];
    expect(banner?.role).toBe('recap');
    expect(banner?.content).toBe('Shipped the fix');
  });

  it('slack events fold into the mirror instead of the task bus', () => {
    bridgeEventToWebBus({
      type: 'slack_message_received',
      channel: 'C1',
      text: 'deploy?',
      tier: 'all',
      user_display: 'Bob',
    } as unknown as AppEvent);
    bridgeEventToWebBus({
      type: 'slack_reply_sent',
      channel: 'C1',
      text: 'on it',
    } as unknown as AppEvent);

    expect(get(slackMirror).map((b) => [b.kind, b.text])).toEqual([
      ['inbound', 'deploy?'],
      ['reply', 'on it'],
    ]);
  });

  it('every event reaches the Events tab log, ping and lag included', () => {
    bridgeEventToWebBus({ type: 'ping' } as AppEvent);
    bridgeEventToWebBus({ type: 'lag' } as AppEvent);
    bridgeEventToWebBus({ type: 'slack_reply_sent', channel: 'C1', text: 'yo' } as unknown as AppEvent);

    expect(get(eventLog).map((r) => r.type)).toEqual(['ping', 'lag', 'slack_reply_sent']);
  });
});
