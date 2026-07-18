// Why: Code-critic review of PR #3028 (MEDIUM) — a session that vanishes
// between `SessionMonitor`'s `GET /sessions/{id}` and
// `GET /sessions/{id}/transcript` calls within the same `refresh()` used to
// leave the card stuck at `phase='ready'` with a stale `session` object (and
// its live Cancel button) showing a raw `HTTP 404` in `error`, instead of
// resetting to the same `no-session` state the detail fetch's own 404 branch
// already produces. This pins the fix: a transcript-fetch 404 must reset
// exactly like a detail-fetch 404 does.
// What: Mounts the real `SessionMonitor.svelte` with `fetch` stubbed to
// answer `GET /sessions` and `GET /sessions/{id}` successfully but
// `GET /sessions/{id}/transcript` with a 404, then waits for the rendered
// text to settle on the `no-session` empty state.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import SessionMonitor from './SessionMonitor.svelte';

const SESSION_ID = 'sess-transcript-404';
const CREATED_AT = new Date().toISOString();

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

beforeEach(() => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            sessions: [
              { id: SESSION_ID, status: 'running', created_at: CREATED_AT, project: null },
            ],
          }),
        } as Response;
      }
      if (url.endsWith(`/sessions/${SESSION_ID}/transcript`)) {
        return {
          ok: false,
          status: 404,
          json: async () => ({ error: { code: -32007, message: 'session not found' } }),
        } as Response;
      }
      if (url.endsWith(`/sessions/${SESSION_ID}`)) {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            id: SESSION_ID,
            status: 'running',
            created_at: CREATED_AT,
            task: 'do the thing',
            agent: null,
            mode: null,
          }),
        } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    }),
  );
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

describe('SessionMonitor transcript-404 reset (review follow-up)', () => {
  it('resets to the no-session state when the transcript fetch 404s after a successful detail fetch', async () => {
    instance = mount(SessionMonitor, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('no active session to monitor') ?? false);

    expect(target.textContent).toContain('no active session to monitor');
    // Must not be stuck showing the raw HTTP error or the stale session's
    // Cancel button — both would indicate the 404 branch was skipped.
    expect(target.textContent).not.toContain('HTTP 404');
    expect(target.querySelector('button')).toBeNull();
  });
});
