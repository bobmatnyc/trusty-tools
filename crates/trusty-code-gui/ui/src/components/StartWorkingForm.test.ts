// Why: `StartWorkingForm.svelte` (renamed from `NewWorkstreamForm.svelte`,
// issue #3384) is the primary GUI entry point — issue #3365/PR #3375's
// explicit "create workstream" ceremony is gone; this file pins that it
// STAYS gone (no "new workstream" header, no "create workstream" button
// text anywhere) while the underlying create -> run -> activate machinery
// is unchanged. Its pure logic is covered by `lib/new-workstream.test.ts`;
// this file covers what only mounting the real component (plus its
// `ProjectPickerModal` child) can: the submit button's disabled/enabled
// states, the no-double-submit guard, the picker-modal wiring (open -> pick
// a roster row / go projectless -> "selected:" line updates), the create ->
// run -> activate submit sequence (code-critic PR #3375 review, HIGH —
// activation now fires ONLY after a successful task-run, never before, and
// a task-run failure after a successful create reuses the same workstream
// id on retry rather than minting another), and the carried-over
// Enter/Shift+Enter keyboard behavior (issue #3132).
// What: Mounts the real component with `fetch` stubbed to answer
// `GET /projects`, `POST /workstreams`, `POST /workstreams/{id}/activate`,
// and `POST /tasks` by URL/method — mirrors this file's own predecessors'
// stub-fetch-by-URL-suffix mounting pattern. `fullSuccessFetch`'s optional
// `callLog` records `{method, url}` per call, in order, so tests can assert
// the CALL SEQUENCE itself (e.g. "no `/activate` call before `/tasks`
// resolves"), not just which routes were eventually hit.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import StartWorkingForm from './StartWorkingForm.svelte';

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
    (b) => b.textContent === 'go' || b.textContent === 'starting…',
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

const ROSTER = {
  entries: [{ name: 'acme-api', path: '/home/bob/acme-api', owner: null }],
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
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
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

  it('issue #3384: no explicit creation ceremony — header reads "start working", submit reads "go", no "new workstream"/"create workstream" text anywhere', () => {
    instance = mount(StartWorkingForm, { target }) as unknown as Record<string, unknown>;

    const heading = target.querySelector('h2');
    expect(heading?.textContent?.trim()).toBe('start working');
    expect(submitButton().textContent).toBe('go');
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
    await waitFor(() => submitButton().disabled && submitButton().textContent === 'starting…');

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

  it('an activation failure does not block the task run — success still reported (best-effort activation)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url.endsWith('/workstreams') && method === 'POST') {
          return { ok: true, status: 201, json: async () => ({ id: 'ws-abc123' }) } as Response;
        }
        if (url.endsWith('/activate') && method === 'POST') {
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
