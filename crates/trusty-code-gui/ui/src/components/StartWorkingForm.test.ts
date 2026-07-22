// Why: `StartWorkingForm.svelte` (renamed from `NewWorkstreamForm.svelte`,
// issue #3384) is the primary GUI entry point — issue #3365/PR #3375's
// explicit "create workstream" ceremony is gone; this file pins that it
// STAYS gone (no "new workstream"/"create workstream" text anywhere) while
// the underlying create -> run -> activate machinery is unchanged. Issue
// #3446 turns it into a bottom-docked chat input bar ("send"/"sending…"
// replace "go"/"starting…", no card header) and adds chat continuation
// (after the first successful task, a subsequent submit resumes the SAME
// session via `session_id` rather than minting a new workstream). Issue
// #3447 fixes project-selection persistence (a picked project no longer
// resets to projectless on a successful submit) — solved as ONE state model
// with continuation (`lib/selected-project.svelte.ts`'s shared store; a
// project CHANGE is what resets continuation, not a successful submit).
// Its pure logic is covered by `lib/new-workstream.test.ts`; this file
// covers what only mounting the real component (plus its
// `ProjectPickerModal` child) can: the submit button's disabled/enabled
// states, the no-double-submit guard, the picker-modal wiring, the create ->
// run -> activate submit sequence (code-critic PR #3375 review, HIGH —
// activation fires ONLY after a successful task-run, and a task-run failure
// after a successful create reuses the same workstream id on retry), the
// carried-over Enter/Shift+Enter keyboard behavior (issue #3132), project-
// selection PERSISTENCE across a successful submit (issue #3447 bug 1), and
// chat continuation (issue #3446): the session-reuse-first sequence, the
// rejected-reuse DISAMBIGUATION probe (code-critic PR #3460 review, HIGH 1
// — verified-terminal falls back visibly, still-running BLOCKS instead of
// forking an orphan, unverifiable refuses to mint blind), the reset-on-
// project-change rule, and the re-target-on-active-workstream-change rule
// (code-critic PR #3460 review, HIGH 2 — including the store-catch-up
// non-reset case).
// What: Mounts the real component with `fetch` stubbed to answer
// `GET /projects`, `POST /workstreams`, `POST /workstreams/{id}/activate`,
// and `POST /tasks` by URL/method — mirrors this file's own predecessors'
// stub-fetch-by-URL-suffix mounting pattern. `fullSuccessFetch`'s optional
// `callLog` records `{method, url}` per call, in order, so tests can assert
// the CALL SEQUENCE itself (e.g. "no `/activate` call before `/tasks`
// resolves"), not just which routes were eventually hit. Resets the shared
// `selectedProjectState` store between tests (module-level state persists
// across tests in the same file otherwise).
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import StartWorkingForm from './StartWorkingForm.svelte';
import { setActiveWorkstreamId } from '../lib/active-workstream.svelte';
import { clearPendingWorkstream, pendingWorkstream } from '../lib/pending-workstream.svelte';
import { selectProject } from '../lib/selected-project.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('timed out waiting for condition');
}

function submitButton(): HTMLButtonElement {
  const button = Array.from(target.querySelectorAll('button')).find(
    (b) => b.textContent === 'send' || b.textContent === 'sending…',
  );
  if (!button) throw new Error('submit button not found');
  return button as HTMLButtonElement;
}

function taskField(): HTMLTextAreaElement {
  return target.querySelector('#start-working-task') as HTMLTextAreaElement;
}

function openPickerButton(): HTMLButtonElement {
  return Array.from(target.querySelectorAll('button')).find(
    (b) => b.textContent === 'choose project',
  ) as HTMLButtonElement;
}

/** Whether a button with exactly this trimmed text is currently rendered —
 * used where a control's ABSENCE (not just its presence) is the assertion,
 * e.g. the project picker/clear controls once bound (issue #3446 + #3447),
 * so no unsafe cast on a possibly-missing element is needed. */
function hasButtonWithText(text: string): boolean {
  return Array.from(target.querySelectorAll('button')).some((b) => b.textContent?.trim() === text);
}

const ROSTER = {
  entries: [{ name: 'acme-api', path: '/home/bob/acme-api', owner: null, registered: true }],
  source: 'registry',
};

/** A `fetch` stub covering every route this component's full submit
 * sequence touches, all succeeding. Individual tests override specific
 * routes to exercise failure paths. `callLog`, when passed, records every
 * call as `{method, url-suffix}` in the order `fetch` was invoked — used to
 * assert call ORDER (e.g. `/activate` never precedes a successful
 * `/tasks` call), not just which routes were eventually hit. */
function fullSuccessFetch(opts?: { tasksBody?: unknown; callLog?: string[] }) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? 'GET';
    opts?.callLog?.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
    if (url.includes('/projects')) {
      return { ok: true, status: 200, json: async () => ROSTER } as Response;
    }
    if (url.endsWith('/workstreams') && method === 'POST') {
      return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
    }
    if (url.endsWith('/activate') && method === 'POST') {
      return { ok: true, status: 200, json: async () => ({ active_id: 'ws-abc123' }) } as Response;
    }
    if (url.endsWith('/tasks') && method === 'POST') {
      if (opts) opts.tasksBody = JSON.parse(init!.body as string);
      return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
    }
    throw new Error(`unexpected fetch: ${method} ${url}`);
  });
}

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
  // The pending-workstream fallback marker (code-critic PR #3392 review,
  // MEDIUM), the shared project selection (issue #3447), and the shared
  // active-workstream id (code-critic PR #3460 review, HIGH 2) are all
  // MODULE-level stores shared across every test in this file — reset them
  // so one test's mint/selection never leaks into the next.
  clearPendingWorkstream();
  selectProject(null);
  setActiveWorkstreamId(null);
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  selectProject(null);
  setActiveWorkstreamId(null);
  vi.unstubAllGlobals();
});

describe('StartWorkingForm submit gating', () => {
  it('disables submit when the task is empty, enables it once text is entered', async () => {
    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;

    expect(submitButton().disabled).toBe(true);

    taskField().value = 'fix the login bug';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    expect(submitButton().disabled).toBe(false);
  });

  it('renders "projectless" as the default selection with no network call at mount', () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => {
        throw new Error('no fetch should happen at mount');
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    expect(target.textContent).toContain('projectless');
  });

  it('issue #3384/#3446: no explicit creation ceremony and no card header — a bare docked input bar, submit reads "send", no "new workstream"/"create workstream"/"start working" heading text anywhere', () => {
    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;

    expect(target.querySelector('h2')).toBeNull();
    expect(submitButton().textContent).toBe('send');
    expect(target.textContent).not.toContain('new workstream');
    expect(target.textContent).not.toContain('create workstream');
  });
});

describe('StartWorkingForm project picker wiring', () => {
  it('opens the modal, and picking a roster row updates the selected-project line', async () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    openPickerButton().click();

    await waitFor(() => target.textContent?.includes('acme-api') ?? false);
    const row = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement;
    row.click();

    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);
    expect(target.textContent).toContain('git repo — acme-api');
  });

  it('"start chatting without a project" closes the modal and keeps the selection projectless', async () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    openPickerButton().click();

    await waitFor(
      () => target.textContent?.includes('start chatting without a project') ?? false,
    );
    const button = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'start chatting without a project',
    ) as HTMLButtonElement;
    button.click();

    await waitFor(() => target.querySelector('[role="dialog"]') === null);
    expect(target.textContent).toContain('projectless');
  });

  it('the "clear" button resets a selection back to projectless', async () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    openPickerButton().click();
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);
    (Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement).click();
    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);

    const clearButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'clear',
    ) as HTMLButtonElement;
    clearButton.click();

    await waitFor(() => target.textContent?.includes('projectless') ?? false);
    expect(target.textContent).toContain('projectless');
  });
});

describe('StartWorkingForm submit sequence', () => {
  it('creates a workstream, runs the task, and activates it — plain "started" message, no ceremony/id exposed', async () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    // Issue #3384: no "new workstream"/"create workstream" ceremony
    // language anywhere, and no internal session id exposed.
    expect(target.textContent).not.toContain('new workstream');
    expect(target.textContent).not.toContain('create workstream');
    expect(target.textContent).not.toContain('sess-abc');
    expect(taskField().value).toBe('');
    expect(submitButton().disabled).toBe(true); // task cleared on success
  });

  it('forwards the selected project and the newly-created workstream_id in the POST /tasks body', async () => {
    const opts: { tasksBody: unknown } = { tasksBody: null };
    vi.stubGlobal('fetch', fullSuccessFetch(opts));

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    openPickerButton().click();
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);
    (Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement).click();
    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    expect(opts.tasksBody).toEqual({
      task_description: 'ship the feature',
      project: '/home/bob/acme-api',
      workstream_id: 'ws-abc123',
    });
  });

  it('calls create -> run -> activate in that order, never activating before a successful task-run', async () => {
    const callLog: string[] = [];
    vi.stubGlobal('fetch', fullSuccessFetch({ callLog }));

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    expect(callLog).toEqual([
      'POST /workstreams',
      'POST /tasks',
      'POST /workstreams/ws-abc123/activate',
    ]);
  });

  it('disables submit while the sequence is in flight (no double submit)', async () => {
    let resolveTasks: ((value: Response) => void) | null = null;
    const tasksPromise = new Promise<Response>((resolve) => {
      resolveTasks = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          return tasksPromise;
        }
        throw new Error(`unexpected fetch: ${method} ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => submitButton().disabled && submitButton().textContent === 'sending…');

    // A second click while in flight must be a no-op.
    submitButton().click();

    resolveTasks!({
      ok: true,
      status: 202,
      json: async () => ({ session_id: 'sess-abc12345' }),
    } as Response);

    await waitFor(() => target.textContent?.includes('started') ?? false);
    expect(submitButton().disabled).toBe(true); // task cleared on success
  });

  it('surfaces a daemon error and does not proceed when POST /workstreams fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/workstreams') && init?.method === 'POST') {
          return {
            ok: false,
            status: 500,
            json: async () => ({ error: { message: 'internal error' } }),
          } as Response;
        }
        throw new Error(`unexpected fetch: ${url} (workstream creation must be the only call)`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('internal error') ?? false);

    expect(taskField().value).toBe('ship the feature'); // not cleared on error
    expect(submitButton().disabled).toBe(false); // re-enabled after failure
  });

  it('surfaces a daemon error from POST /tasks after the workstream was already created — names the workstream, never activates, and a retry reuses it (code-critic PR #3375 review, HIGH)', async () => {
    const callLog: string[] = [];
    let taskCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          // Must never be reached on either attempt below — activation is
          // the LAST step, only after a successful task-run.
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          taskCallCount += 1;
          return {
            ok: false,
            status: 400,
            json: async () => ({ error: { message: 'task must not be empty' } }),
          } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('task must not be empty') ?? false);

    expect(taskField().value).toBe('ship the feature'); // not cleared on error
    expect(submitButton().disabled).toBe(false); // re-enabled after failure
    // The error names the already-created workstream (HIGH finding: the
    // operator must know it exists, not just that the task failed).
    expect(target.textContent).toContain('was created but not activated');
    expect(callLog).toEqual(['POST /workstreams', 'POST /tasks']);
    expect(callLog.some((c) => c.includes('/activate'))).toBe(false);

    // Retry: must reuse the SAME workstream id — no second POST
    // /workstreams — and must still never activate (task fails again here).
    submitButton().click();
    await waitFor(() => taskCallCount === 2);

    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
    expect(callLog.some((c) => c.includes('/activate'))).toBe(false);
  });

  it('a retry that succeeds reuses the same workstream_id in POST /tasks and activates exactly once', async () => {
    const callLog: string[] = [];
    const taskBodies: unknown[] = [];
    let taskCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          taskCallCount += 1;
          taskBodies.push(JSON.parse(init!.body as string));
          if (taskCallCount === 1) {
            return {
              ok: false,
              status: 400,
              json: async () => ({ error: { message: 'transient failure' } }),
            } as Response;
          }
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('was created but not activated') ?? false);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    // Exactly one workstream minted, reused by both /tasks calls; exactly
    // one activation, and it happens only after the SECOND (successful)
    // /tasks call.
    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
    expect(callLog.filter((c) => c.includes('/activate'))).toHaveLength(1);
    expect(callLog.indexOf('POST /workstreams/ws-abc123/activate')).toBeGreaterThan(
      callLog.lastIndexOf('POST /tasks'),
    );
    expect(taskBodies).toEqual([
      { task_description: 'ship the feature', workstream_id: 'ws-abc123' },
      { task_description: 'ship the feature', workstream_id: 'ws-abc123' },
    ]);
  });

  it('an activation failure retries ONCE, then does not block the task run — success still reported, but the pending-workstream fallback marker stays set (code-critic PR #3392 review, MEDIUM)', async () => {
    let activateCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          activateCallCount += 1;
          return { ok: false, status: 409, json: async () => ({ error: { message: 'conflict' } }) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    expect(target.textContent).toContain('started');
    expect(target.textContent).toContain('could not activate');
    expect(taskField().value).toBe(''); // still cleared — the sequence still succeeded overall
    expect(activateCallCount).toBe(2); // exactly one retry, not unbounded
    // The fallback marker stays SET on a genuine (both-attempts) failure —
    // `WorkstreamActivity.svelte` needs it to still be there.
    expect(pendingWorkstream.id).toBe('ws-abc123');
    expect(pendingWorkstream.name).toBeTruthy();
  });

  it('an activation failure that succeeds on the retry surfaces no warning and clears the pending-workstream marker', async () => {
    let activateCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          activateCallCount += 1;
          if (activateCallCount === 1) {
            return { ok: false, status: 409, json: async () => ({ error: { message: 'conflict' } }) } as Response;
          }
          return { ok: true, status: 200, json: async () => ({ active_id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    expect(activateCallCount).toBe(2);
    expect(target.textContent).not.toContain('could not activate');
    // Activation ultimately succeeded — the fallback marker is no longer
    // needed; the daemon's real active pointer takes over on the next poll.
    expect(pendingWorkstream.id).toBeNull();
    expect(pendingWorkstream.name).toBeNull();
  });

  it('sets the pending-workstream fallback marker as soon as the workstream is minted, BEFORE the task-run resolves', async () => {
    let capturedDuringTaskRun: { id: string | null; name: string | null } | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          capturedDuringTaskRun = { id: pendingWorkstream.id, name: pendingWorkstream.name };
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    expect(capturedDuringTaskRun).toEqual({ id: 'ws-abc123', name: expect.any(String) });
  });
});

describe('StartWorkingForm project-selection persistence + chat continuation (issues #3447 bug 1 + #3446 — one state model)', () => {
  it('issue #3447 bug 1: a picked project stays selected after a successful submit — no longer resets to projectless', async () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    openPickerButton().click();
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);
    (Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement).click();
    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    expect(target.textContent).toContain('git repo — acme-api');
    expect(target.textContent).not.toContain('projectless');
  });

  it('issue #3446: after the first task, a second submit continues the SAME session via session_id — no second POST /workstreams, no second /activate', async () => {
    const callLog: string[] = [];
    const taskBodies: unknown[] = [];
    let taskCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          taskCallCount += 1;
          taskBodies.push(JSON.parse(init!.body as string));
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    taskField().value = 'second message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => taskCallCount === 2);

    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
    expect(callLog.filter((c) => c.includes('/activate'))).toHaveLength(1);
    expect(taskBodies[1]).toEqual({
      task_description: 'second message',
      session_id: 'sess-abc12345',
    });
  });

  it('issue #3446 + code-critic PR #3460 HIGH 1: a reuse rejection whose session is VERIFIED terminal falls back to a fresh session under the SAME workstream, with a visible notice — still no second POST /workstreams', async () => {
    const callLog: string[] = [];
    let nonReuseCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/sessions/sess-abc12345') && method === 'GET') {
          // The disambiguation probe — the session genuinely ended.
          return {
            ok: true,
            status: 200,
            json: async () => ({ id: 'sess-abc12345', status: 'cancelled' }),
          } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          const body = JSON.parse(init!.body as string) as { session_id?: string };
          if (body.session_id) {
            // The reuse attempt — rejected (daemon does not say which cause).
            return {
              ok: false,
              status: 409,
              json: async () => ({ error: { message: 'session already terminal' } }),
            } as Response;
          }
          nonReuseCallCount += 1;
          const sessionId = nonReuseCallCount === 1 ? 'sess-abc12345' : 'sess-def67890';
          return { ok: true, status: 202, json: async () => ({ session_id: sessionId }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    taskField().value = 'second message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => callLog.filter((c) => c === 'POST /tasks').length === 3);

    // Call order: mint's /tasks, then the second submit's reuse attempt
    // (rejected), then the verification probe, then the fallback mint —
    // never a second POST /workstreams.
    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
    expect(callLog).toContain('GET /sessions/sess-abc12345');
    expect(nonReuseCallCount).toBe(2);
    // The fallback mint is VISIBLE, not silent (HIGH 1's minimum bar).
    await waitFor(() => target.textContent?.includes('previous session ended (cancelled)') ?? false);
  });

  it('code-critic PR #3460 HIGH 1: a reuse rejection whose session is still RUNNING blocks the submit — no fallback mint, no orphaned concurrent session', async () => {
    const callLog: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/sessions/sess-abc12345') && method === 'GET') {
          // The disambiguation probe — the first task is STILL RUNNING
          // (task.run is 202-then-background; a quick follow-up hits this
          // routinely, not exotically).
          return {
            ok: true,
            status: 200,
            json: async () => ({ id: 'sess-abc12345', status: 'running' }),
          } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          const body = JSON.parse(init!.body as string) as { session_id?: string };
          if (body.session_id) {
            // Same undifferentiated rejection the daemon sends for
            // "already has a task running".
            return {
              ok: false,
              status: 409,
              json: async () => ({ error: { message: 'session sess-abc12345 already has a task running' } }),
            } as Response;
          }
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    taskField().value = 'quick follow-up';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('still running') ?? false);

    // Exactly TWO POST /tasks total (the mint + the rejected reuse) — the
    // fallback mint must NOT have fired, and no second workstream either.
    expect(callLog.filter((c) => c === 'POST /tasks')).toHaveLength(2);
    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
    // The task text is preserved for a later retry, and the form re-enables.
    expect(taskField().value).toBe('quick follow-up');
    await waitFor(() => !submitButton().disabled);
  });

  it('code-critic PR #3460 HIGH 1: an UNVERIFIABLE probe (network failure) refuses to mint blind — error surfaced, no fallback mint', async () => {
    const callLog: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/sessions/sess-abc12345') && method === 'GET') {
          throw new TypeError('Failed to fetch');
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          const body = JSON.parse(init!.body as string) as { session_id?: string };
          if (body.session_id) {
            return {
              ok: false,
              status: 409,
              json: async () => ({ error: { message: 'rejected' } }),
            } as Response;
          }
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    taskField().value = 'follow-up';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('could not verify') ?? false);

    expect(callLog.filter((c) => c === 'POST /tasks')).toHaveLength(2); // mint + rejected reuse only
    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
  });

  it('code-critic PR #3460 HIGH 2: when the ACTIVE workstream changes (e.g. via the header switcher), the next submit targets the NEW workstream — not the old session, and no new POST /workstreams', async () => {
    const callLog: string[] = [];
    const taskBodies: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        callLog.push(`${method} ${url.replace(/^https?:\/\/[^/]+/, '')}`);
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          taskBodies.push(JSON.parse(init!.body as string));
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message (in the minted workstream)';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    // The daemon's active workstream changes under the form — exactly what
    // WorkstreamSwitcher/WorkstreamActivity publish after a header switch.
    setActiveWorkstreamId('ws-OTHER');
    flushSync();

    taskField().value = 'follow-up after switching';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => taskBodies.length === 2);

    // The follow-up runs under the workstream the operator switched TO —
    // never session-A reuse, never a surprise third workstream.
    expect(taskBodies[1]).toEqual({
      task_description: 'follow-up after switching',
      workstream_id: 'ws-OTHER',
    });
    expect(callLog.filter((c) => c === 'POST /workstreams')).toHaveLength(1);
  });

  it('code-critic PR #3460 HIGH 2: the store merely catching up to the workstream this form itself just minted does NOT reset continuation', async () => {
    const taskBodies: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          taskBodies.push(JSON.parse(init!.body as string));
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    // A poll tick lands, reporting the workstream this form just minted and
    // activated — that is the store CATCHING UP, not a switch.
    setActiveWorkstreamId('ws-abc123');
    flushSync();

    taskField().value = 'second message';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => taskBodies.length === 2);

    // Continuation survived: session reuse, not a workstream-targeted mint.
    expect(taskBodies[1]).toEqual({
      task_description: 'second message',
      session_id: 'sess-abc12345',
    });
  });

  it('issue #3447 regression: pick a project, submit, submit again — both tasks carry the same project + workstream/session, and the selection stays shown', async () => {
    const taskBodies: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.includes('/projects')) {
          return { ok: true, status: 200, json: async () => ROSTER } as Response;
        }
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          taskBodies.push(JSON.parse(init!.body as string));
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc12345' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    openPickerButton().click();
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);
    (Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement).click();
    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);

    taskField().value = 'first task';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    taskField().value = 'second task';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => taskBodies.length === 2);

    expect(target.textContent).toContain('git repo — acme-api');
    expect(taskBodies[0]).toEqual({
      task_description: 'first task',
      project: '/home/bob/acme-api',
      workstream_id: 'ws-abc123',
    });
    expect(taskBodies[1]).toEqual({
      task_description: 'second task',
      project: '/home/bob/acme-api',
      session_id: 'sess-abc12345',
    });
  });

  it('issue #3446 + #3447: the project picker/clear controls are hidden once this conversation is bound to a workstream — a project is chosen at creation and then locked (Bob: "new workstream if we want to change projects")', async () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    // Before the first submit — nothing bound yet — the picker is offered.
    expect(hasButtonWithText('choose project')).toBe(true);

    taskField().value = 'first message (projectless)';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);

    // Bound now (continuationWorkstreamId set by the successful submit) —
    // "choose project" must be gone; there was no project to "clear" to
    // begin with.
    await waitFor(() => !hasButtonWithText('choose project'));
    expect(hasButtonWithText('clear')).toBe(false);
  });

  it('a project change from an EXTERNAL surface (e.g. the rail\'s own project picker, via the shared selected-project store) still resets continuation — the next submit mints a fresh workstream, not a second POST /workstreams-free continuation', async () => {
    let wsCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.includes('/projects')) {
          return { ok: true, status: 200, json: async () => ROSTER } as Response;
        }
        if (url.endsWith('/workstreams') && method === 'POST') {
          wsCallCount += 1;
          return { ok: true, status: 201, json: async () => ({ id: `ws-${wsCallCount}` }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          return { ok: true, status: 202, json: async () => ({ session_id: `sess-${wsCallCount}` }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'first message (projectless)';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => target.textContent?.includes('started') ?? false);
    expect(wsCallCount).toBe(1);

    // This form's OWN picker is gone now (bound — see the test above); the
    // shared store can still change from elsewhere (`WorkstreamRail`'s
    // Projects rail view writes to the same `selected-project.svelte.ts`
    // store) — that must still reset continuation (a session's project
    // binding is immutable once set), matching this form's own effect.
    selectProject({ path: '/home/bob/acme-api', displayPath: 'acme-api', isGitRepo: true });
    flushSync();
    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);

    taskField().value = 'second message (now with a project)';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);
    submitButton().click();
    await waitFor(() => wsCallCount === 2);
  });
});

describe('StartWorkingForm keyboard submit (issue #3132, carried over)', () => {
  it('Enter (no Shift) submits the task field, and a rapid second Enter while in flight causes no second POST', async () => {
    let postCount = 0;
    let resolveTasks: ((value: Response) => void) | null = null;
    const tasksPromise = new Promise<Response>((resolve) => {
      resolveTasks = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          postCount += 1;
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
          return { ok: true, status: 200, json: async () => ({}) } as Response;
        }
        if (url.endsWith('/tasks') && method === 'POST') {
          return tasksPromise;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    const firstEnter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    taskField().dispatchEvent(firstEnter);
    expect(firstEnter.defaultPrevented).toBe(true);
    await waitFor(() => postCount === 1 && submitButton().disabled);

    const secondEnter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    taskField().dispatchEvent(secondEnter);
    expect(postCount).toBe(1);

    resolveTasks!({
      ok: true,
      status: 202,
      json: async () => ({ session_id: 'sess-abc12345' }),
    } as Response);

    await waitFor(() => target.textContent?.includes('started') ?? false);
    expect(postCount).toBe(1);
  });

  it('Shift+Enter does not submit, letting the textarea insert a newline', () => {
    vi.stubGlobal('fetch', fullSuccessFetch());

    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;
    taskField().value = 'line one';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));

    const shiftEnter = new KeyboardEvent('keydown', {
      key: 'Enter',
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });
    taskField().dispatchEvent(shiftEnter);

    expect(shiftEnter.defaultPrevented).toBe(false);
  });
});
