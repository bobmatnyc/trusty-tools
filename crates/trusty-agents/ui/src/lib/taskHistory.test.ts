// Unit tests for the Recent Tasks display filter (owner report 2026-07-23).
//
// Why: `GET /api/tasks` returns every persisted `PmResponse`, including the
// per-turn envelope stored for ordinary chat replies (`type: agent_response`
// with an empty `task`). Those rendered as junk "(empty)" success rows in the
// Recent Tasks panel — the owner flagged them live ("I shouldn't see these").
// `isDisplayableTask` is the pure predicate that keeps chat turns out of the
// workflow-oriented panel while preserving the pre-fix visibility rules for
// every other row; pinning its contract here guards the regression without
// mounting the Svelte component or standing up a live sidecar.

import { describe, expect, it } from 'vitest';
import {
  CHAT_RESPONSE_TYPE,
  isDisplayableTask,
  visibleTaskHistory,
} from './taskHistory';

describe('isDisplayableTask (owner report 2026-07-23)', () => {
  it('hides chat replies (agent_response), the exact junk the owner reported', () => {
    // The two live rows: empty task, status success, but an agent_response type.
    expect(
      isDisplayableTask({
        type: CHAT_RESPONSE_TYPE,
        status: 'success',
        narrative: 'Hi Masa. How can I assist?',
      }),
    ).toBe(false);
    expect(
      isDisplayableTask({
        type: 'agent_response',
        status: 'success',
        narrative: "I don't have specific information…",
      }),
    ).toBe(false);
  });

  it('shows a completed workflow run', () => {
    expect(
      isDisplayableTask({
        type: 'workflow_result',
        status: 'success',
        narrative: 'Implemented the feature and all tests pass.',
      }),
    ).toBe(true);
  });

  it('shows a failed (error) run — errors are not chat noise', () => {
    expect(isDisplayableTask({ type: 'error', status: 'error', narrative: 'boom' })).toBe(
      true,
    );
  });

  it('hides the contentless running placeholder, keeps a running run with content', () => {
    expect(
      isDisplayableTask({ type: 'task_submitted', status: 'running', narrative: '' }),
    ).toBe(false);
    expect(
      isDisplayableTask({ type: 'task_submitted', status: 'running', narrative: '   ' }),
    ).toBe(false);
    expect(
      isDisplayableTask({
        type: 'workflow_result',
        status: 'running',
        narrative: 'Researching…',
      }),
    ).toBe(true);
  });

  it('keeps a running task that has a title but no narrative yet (future request-text)', () => {
    // Guards the pre-fix `task?.trim()` disjunct: once the backend populates a
    // request-text `task` field (flagged as a follow-up), a running task with a
    // title but not-yet-a-narrative must still show, not be silently hidden.
    expect(
      isDisplayableTask({
        type: 'task_submitted',
        status: 'running',
        task: 'fix the auth bug',
        narrative: '',
      }),
    ).toBe(true);
  });

  it('shows a legacy row with no type as long as it is not an empty running placeholder', () => {
    // Older persisted snapshots predate the `type` surface; a terminal row with
    // content must still render.
    expect(isDisplayableTask({ status: 'success', narrative: 'done' })).toBe(true);
    expect(isDisplayableTask({ status: 'running', narrative: '' })).toBe(false);
  });
});

describe('visibleTaskHistory', () => {
  it('drops chat rows and preserves order for the rest', () => {
    const rows = [
      { id: 'a', task: '', status: 'success', type: 'workflow_result', narrative: 'built' },
      { id: 'b', task: '', status: 'success', type: 'agent_response', narrative: 'hi' },
      { id: 'c', task: '', status: 'error', type: 'error', narrative: 'boom' },
      { id: 'd', task: '', status: 'running', type: 'task_submitted', narrative: '' },
    ];
    expect(visibleTaskHistory(rows).map((r) => r.id)).toEqual(['a', 'c']);
  });
});
