// Why: `CreateSessionForm.svelte` is this slice's main deliverable — the
// create+prompt flow that closes the "observe-and-cancel only" gap (and, as
// of issue #3177, the "GUI-created sessions are inert" gap — the form now
// calls `POST /tasks`/`task.run`, not the dead-end `POST /sessions`). Its
// pure logic is covered by `lib/create-session.test.ts`; this file covers
// what only mounting the real component can: the submit button's
// disabled/enabled states, the no-double-submit guard under a real in-flight
// `fetch`, and that picking a listed folder updates the "selected:" line
// before submit.
// What: Mounts the real component with `fetch` stubbed to answer
// `GET /fs` and `POST /tasks`. Mirrors `SessionMonitor.test.ts`'s
// stub-fetch-by-URL-suffix mounting pattern.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import CreateSessionForm from './CreateSessionForm.svelte';

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
    (b) => b.textContent === 'create session' || b.textContent === 'creating…',
  );
  if (!button) throw new Error('submit button not found');
  return button as HTMLButtonElement;
}

function taskField(): HTMLTextAreaElement {
  return target.querySelector('#new-session-task') as HTMLTextAreaElement;
}

const HOME_LISTING = {
  path: '/home/bob',
  display_path: '~',
  parent: '/home',
  entries: [
    { name: 'acme-api', path: '/home/bob/acme-api', is_dir: true, is_git_repo: true },
    { name: 'scratch', path: '/home/bob/scratch', is_dir: true, is_git_repo: false },
  ],
};

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

describe('CreateSessionForm submit gating', () => {
  it('disables submit when the task is empty, enables it once text is entered', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    expect(submitButton().disabled).toBe(true);

    taskField().value = 'fix the login bug';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    expect(submitButton().disabled).toBe(false);
  });

  it('picking a listed folder updates the selected-project line', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    expect(target.textContent).toContain('projectless');

    const useButtons = Array.from(target.querySelectorAll('button')).filter(
      (b) => b.textContent === 'use',
    );
    expect(useButtons.length).toBe(2);
    (useButtons[0] as HTMLButtonElement).click();

    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);
    expect(target.textContent).toContain('git repo — acme-api');
  });

  it('disables submit while a create request is in flight (no double submit)', async () => {
    let resolveCreate: ((value: Response) => void) | null = null;
    const createPromise = new Promise<Response>((resolve) => {
      resolveCreate = resolve;
    });

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          return createPromise;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => submitButton().disabled && submitButton().textContent === 'creating…');

    // A second click while in flight must be a no-op — the button is
    // disabled, so the component's own `submit()` guard is never reached a
    // second time.
    submitButton().click();
    expect(submitButton().disabled).toBe(true);

    resolveCreate!({
      ok: true,
      status: 202,
      json: async () => ({ session_id: 'sess-abc12345', status: 'running' }),
    } as Response);

    await waitFor(() => target.textContent?.includes('session created') ?? false);
    expect(submitButton().disabled).toBe(true); // task cleared on success
    expect(taskField().value).toBe('');
  });

  it('surfaces a 400 validation error from the daemon without clearing the form', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          return {
            ok: false,
            status: 400,
            json: async () => ({ error: { code: -32003, message: 'task must not be empty' } }),
          } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('task must not be empty') ?? false);

    expect(taskField().value).toBe('ship the feature'); // not cleared on error
    expect(submitButton().disabled).toBe(false); // re-enabled after failure
  });

  it('surfaces the per-call project-mismatch 400 from the daemon verbatim (#3178, PR #3189)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          return {
            ok: false,
            status: 400,
            json: async () => ({
              error: {
                code: -32003,
                message: "task.run: project `/tmp/other` does not match session `sess-1`'s existing binding",
              },
            }),
          } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('does not match session') ?? false);

    expect(taskField().value).toBe('ship the feature'); // not cleared on error
    expect(submitButton().disabled).toBe(false); // re-enabled after failure
  });

  it('forwards the selected project path in the POST /tasks body', async () => {
    let capturedBody: unknown = null;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          capturedBody = JSON.parse(init.body as string);
          return {
            ok: true,
            status: 202,
            json: async () => ({ session_id: 'sess-abc12345', status: 'running' }),
          } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    const useButtons = Array.from(target.querySelectorAll('button')).filter(
      (b) => b.textContent === 'use',
    );
    (useButtons[0] as HTMLButtonElement).click();
    await waitFor(() => target.textContent?.includes('git repo — acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('session created') ?? false);

    expect(capturedBody).toEqual({
      task_description: 'ship the feature',
      project: '/home/bob/acme-api',
    });
  });

  it('degrades to the error UI when GET /fs returns 200 with a shape-invalid body (PR #3103 HIGH finding)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/fs')) {
          // 200 but no `entries` array — schema drift / interposed proxy.
          return { ok: true, status: 200, json: async () => ({ ok: true }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('malformed response') ?? false);

    // The shell survives: the form renders its own error line instead of the
    // `listing.entries` $derived throwing out of the component tree.
    expect(target.textContent).toContain('malformed response');
    expect(submitButton().disabled).toBe(true); // no task entered yet
  });

  it('shows a generic success message when a 202 body lacks a valid session_id (PR #3103 MEDIUM finding)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          // 202 (task.run was accepted) but the body carries no `session_id`.
          return { ok: true, status: 202, json: async () => ({ status: 'running' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    submitButton().click();
    await waitFor(() => target.textContent?.includes('session created') ?? false);

    expect(target.textContent).toContain('session created');
    expect(target.textContent).not.toContain('session created —'); // no id prefix
    expect(taskField().value).toBe(''); // 202 is authoritative — form still clears
  });

  it('issue #3132: Enter (no Shift) submits the task field, and a rapid second Enter while in flight causes no second POST', async () => {
    let postCount = 0;
    let resolveCreate: ((value: Response) => void) | null = null;
    const createPromise = new Promise<Response>((resolve) => {
      resolveCreate = resolve;
    });

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          postCount += 1;
          return createPromise;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    taskField().value = 'ship the feature';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    const firstEnter = new KeyboardEvent('keydown', {
      key: 'Enter',
      bubbles: true,
      cancelable: true,
    });
    taskField().dispatchEvent(firstEnter);

    // Intercepted to submit rather than insert a newline.
    expect(firstEnter.defaultPrevented).toBe(true);
    await waitFor(() => postCount === 1 && submitButton().disabled);

    // A rapid second Enter while the first submit is still in flight must
    // be a no-op — `submit()`'s own `canSubmitCreate` guard (submitPhase
    // already 'submitting') rejects it before a second fetch is ever made,
    // same guard the button's `disabled` binding relies on.
    const secondEnter = new KeyboardEvent('keydown', {
      key: 'Enter',
      bubbles: true,
      cancelable: true,
    });
    taskField().dispatchEvent(secondEnter);
    expect(postCount).toBe(1);

    resolveCreate!({
      ok: true,
      status: 202,
      json: async () => ({ session_id: 'sess-abc12345', status: 'running' }),
    } as Response);

    await waitFor(() => target.textContent?.includes('session created') ?? false);
    expect(postCount).toBe(1);
  });

  it('issue #3132: Shift+Enter does not submit, letting the textarea insert a newline', async () => {
    let postCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        if (url.endsWith('/tasks') && init?.method === 'POST') {
          postCount += 1;
          return { ok: true, status: 202, json: async () => ({ session_id: 'sess-abc' }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    taskField().value = 'line one';
    taskField().dispatchEvent(new Event('input', { bubbles: true }));
    await waitFor(() => !submitButton().disabled);

    const shiftEnter = new KeyboardEvent('keydown', {
      key: 'Enter',
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });
    taskField().dispatchEvent(shiftEnter);

    // Never prevented — the textarea's default newline-insertion behavior
    // must be left alone for Shift+Enter.
    expect(shiftEnter.defaultPrevented).toBe(false);
    expect(postCount).toBe(0);
    expect(submitButton().disabled).toBe(false); // form untouched, still submittable by other means
  });

  it('issue #3132: a non-Enter key never triggers submit or preventDefault', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    const aKey = new KeyboardEvent('keydown', { key: 'a', bubbles: true, cancelable: true });
    taskField().dispatchEvent(aKey);
    expect(aKey.defaultPrevented).toBe(false);
  });

  it('issue #3134: the picker "use" button is immediately adjacent to the directory-name button, not pushed to the far right', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/fs')) {
          return { ok: true, status: 200, json: async () => HOME_LISTING } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    const nameButton = Array.from(target.querySelectorAll('li button')).find((b) =>
      b.textContent?.includes('acme-api'),
    ) as HTMLButtonElement;
    expect(nameButton).toBeTruthy();

    // Structural adjacency: the "use" button must be the name button's very
    // next element sibling within the row, with nothing spacing them apart.
    const nextSibling = nameButton.nextElementSibling as HTMLButtonElement | null;
    expect(nextSibling?.textContent?.trim()).toBe('use');

    // The layout classes that caused the far-right separation (stretching
    // the name button to fill the row, then justifying the row's content
    // apart) must be gone — regression guard for issue #3134.
    expect(nameButton.className).not.toMatch(/\bflex-1\b/);
    const row = nameButton.closest('li');
    expect(row?.className).not.toMatch(/justify-between/);
  });

  it('renders daemon-unreachable when the initial GET /fs fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('Failed to fetch');
      }),
    );

    instance = mount(CreateSessionForm, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('daemon unreachable') ?? false);

    expect(target.textContent).toContain('daemon unreachable');
    expect(submitButton().disabled).toBe(true); // no task entered yet
  });
});
