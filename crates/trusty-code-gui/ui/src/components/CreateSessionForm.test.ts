// Why: `CreateSessionForm.svelte` is this slice's main deliverable — the
// create-session flow that closes the "observe-and-cancel only" gap. Its
// pure logic is covered by `lib/create-session.test.ts`; this file covers
// what only mounting the real component can: the submit button's
// disabled/enabled states, the no-double-submit guard under a real in-flight
// `fetch`, and that picking a listed folder updates the "selected:" line
// before submit.
// What: Mounts the real component with `fetch` stubbed to answer
// `GET /fs` and `POST /sessions`. Mirrors `SessionMonitor.test.ts`'s
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
        if (url.endsWith('/sessions') && init?.method === 'POST') {
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
      status: 201,
      json: async () => ({ id: 'sess-abc12345', status: 'running' }),
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
        if (url.endsWith('/sessions') && init?.method === 'POST') {
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
