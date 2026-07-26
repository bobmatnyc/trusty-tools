// #3759 — the browser-mode task-complete flash race, pinned against the real
// `ChatView`.
//
// Why: `session_done` (SSE lifecycle) and the poll loop's `task-complete`
// (`transport.ts::send_message`) both landed on the same web bus for the same
// task id, and `ChatView`'s handler is last-writer-wins. Because `session_done`
// carried a fabricated `narrative: ''`, whichever arrived last decided whether
// the user saw the real reply or "(no narrative)" — and with token streaming
// (#3753) `session_done` normally arrives FIRST, one poll interval (250–500ms)
// ahead of the real text, so freshly streamed prose visibly blanked mid-read.
// A bus-level assertion alone (`lib/eventBridge.test.ts`) can't express that:
// the defect is what the reader SEES, so these mount the component and read
// the rendered bubble. Both interleavings are asserted because the fix must be
// order-independent, not merely tuned for the ordering that happens to be
// common today.
// What: Mounts `ChatView` (browser transport — no Tauri globals under jsdom,
// so `listenEvent` binds to the in-process web bus), seeds one assistant
// bubble tagged with a task id, then replays each ordering, following
// `ChatPane.test.ts`'s mounting pattern.
// Test: this file.

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import ChatView from './ChatView.svelte';
import { bridgeEventToWebBus } from '../lib/eventBridge';
import { emitWebEvent, type AppEvent } from '../lib/transport';
import { streamAccumulator } from '../lib/chatStream';
import { resetWorkflow } from '../stores/workflow';
import { activeProjectId, addMessage, isRunning, messages } from '../stores/app';

const PROJECT = 'ctrl';
const TASK = 'task-1';
const STREAMED = 'The streamed answer so far';
const REAL = 'The authoritative final narrative.';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error('timed out waiting for condition');
}

/** The rendered text of the assistant bubble under test. */
function bubbleText(): string {
  const paras = Array.from(target.querySelectorAll('p.whitespace-pre-wrap'));
  return paras.map((p) => p.textContent ?? '').join('\n');
}

/** Settle Svelte's render queue so `bubbleText()` reflects the last emit. */
async function rendered(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** The SSE lifecycle event that used to fabricate an empty narrative. */
function sessionDone(): void {
  bridgeEventToWebBus({ type: 'session_done', session_id: TASK, status: 'success' } as AppEvent);
}

/** The poll loop's real completion, mirroring `transport.ts::send_message`. */
function pollComplete(): void {
  emitWebEvent('task-complete', { id: TASK, narrative: REAL, status: 'success' });
}

beforeEach(async () => {
  messages.set(new Map());
  activeProjectId.set(PROJECT);
  isRunning.set(true);
  streamAccumulator.finalize(TASK);
  resetWorkflow();

  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(ChatView, { target }) as Record<string, unknown>;

  addMessage(PROJECT, {
    id: 'asst-1',
    role: 'assistant',
    content: '',
    timestamp: 0,
    taskId: TASK,
    speaker: 'Assistant',
  });

  // The component wires its bus listeners from an async `onMount`; emitting
  // before they are attached would silently drop the events under test.
  await waitFor(() => {
    emitWebEvent('task-delta', { task_id: TASK, text: STREAMED, agent: '', done: false });
    return bubbleText().includes(STREAMED);
  });
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  target.remove();
});

describe('task-complete race (#3759)', () => {
  it('session_done first: never blanks the streamed text, then shows the real narrative', async () => {
    // The common ordering. Before the fix this rendered "(no narrative)" for
    // one poll interval, wiping text the user was mid-way through reading.
    sessionDone();
    await rendered();

    expect(bubbleText()).toContain(STREAMED);
    expect(bubbleText()).not.toContain('(no narrative)');

    pollComplete();
    await rendered();

    expect(bubbleText()).toContain(REAL);
  });

  it('real narrative first: a following session_done does not overwrite it', async () => {
    // The reverse interleaving (a slow SSE hop, or a fast first poll). Before
    // the fix this ended on "(no narrative)" permanently — no later event ever
    // arrives to repair it.
    pollComplete();
    await rendered();

    expect(bubbleText()).toContain(REAL);

    sessionDone();
    await rendered();

    expect(bubbleText()).toContain(REAL);
    expect(bubbleText()).not.toContain('(no narrative)');
  });

  it('a foreign session finishing mid-turn leaves this tab alone', async () => {
    // `/api/events` is a process-wide broadcast, so a task started elsewhere
    // (Telegram, another tab) delivers its `session_done` here too. It must
    // touch neither the bubble nor the spinner.
    bridgeEventToWebBus({
      type: 'session_done',
      session_id: 'someone-elses-task',
      status: 'success',
    } as AppEvent);
    await rendered();

    expect(bubbleText()).toContain(STREAMED);
    expect(target.querySelector('[role="status"]')).not.toBeNull();
  });

  it('an empty narrative from the authoritative emitter is still reported', async () => {
    // The guard is "session_done carries no narrative", NOT "empty narratives
    // are ignored" — a genuinely empty result from the poll loop must still
    // read as such rather than freezing on stale streamed text.
    emitWebEvent('task-complete', { id: TASK, narrative: '', status: 'success' });
    await rendered();

    expect(bubbleText()).toContain('(no narrative)');
  });
});
