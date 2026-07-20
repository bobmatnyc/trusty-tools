// Why: `WorkstreamActivity.svelte` (renamed from `SessionMonitor.svelte`,
// issue #3384 Scope B) reframes this card around the ACTIVE WORKSTREAM. This
// file covers what only mounting the real component can: the four phases
// (`connecting`/`daemon-unreachable`/`no-workstream`/`ready`), the
// `no-workstream` empty state's exact wording ("no active workstream — pick
// a project to start", verbatim from the issue), that a session bound to a
// DIFFERENT workstream (or no workstream at all) is NEVER shown as the
// active workstream's activity even when it is the most recently created
// session overall (the regression the old daemon-wide `pickActiveSession`
// heuristic was exposed to), the "workstream active but nothing bound yet"
// sub-empty-state, cancel's two-step confirm, and the transcript-404-reset
// regression PR #3028's code-critic review originally pinned (still
// exercised here, just through the new two-poll chain).
// What: A small in-memory fake daemon backs a stubbed global `fetch`,
// mirroring `WorkstreamSwitcher.test.ts`'s `fakeDaemon` shape — extended to
// also answer `GET /sessions`, `GET /sessions/{id}`,
// `GET /sessions/{id}/transcript`, and `POST /sessions/{id}/cancel`.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import WorkstreamActivity from './WorkstreamActivity.svelte';
import type { Workstream } from '../lib/workstreams';

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

function ws(id: string, name: string, sessionIds: string[]): Workstream {
  return {
    id,
    name,
    state: 'active',
    session_ids: sessionIds,
    created_at: '2026-07-20T14:00:00Z',
    updated_at: '2026-07-20T14:00:00Z',
    metadata: {},
  };
}

interface FakeSession {
  id: string;
  status: string;
  created_at: string;
  project: string | null;
  task: string;
  agent: string | null;
  mode: string | null;
}

function session(id: string, status: string, created_at: string, task = 'do the thing'): FakeSession {
  return { id, status, created_at, project: null, task, agent: null, mode: null };
}

/** A minimal in-memory fake of `/workstreams` + `/sessions*`. `transcripts`
 * defaults every session to an empty turn list unless overridden;
 * `transcript404For` makes ONE session id's transcript fetch 404 instead
 * (the PR #3028 regression case). */
function fakeDaemon(opts: {
  activeWorkstreamId: string | null;
  workstreams: Workstream[];
  sessions: FakeSession[];
  transcript404For?: string;
}) {
  const cancelled = new Set<string>();
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? 'GET';

    if (url.endsWith('/workstreams')) {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          active_workstream_id: opts.activeWorkstreamId,
          workstreams: opts.workstreams,
        }),
      } as Response;
    }
    if (url.endsWith('/sessions')) {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          sessions: opts.sessions.map((s) => ({
            ...s,
            status: cancelled.has(s.id) ? 'cancelled' : s.status,
          })),
        }),
      } as Response;
    }
    const transcriptMatch = url.match(/\/sessions\/([^/]+)\/transcript$/);
    if (transcriptMatch) {
      if (transcriptMatch[1] === opts.transcript404For) {
        return { ok: false, status: 404, json: async () => ({}) } as Response;
      }
      return { ok: true, status: 200, json: async () => ({ turns: [] }) } as Response;
    }
    const cancelMatch = url.match(/\/sessions\/([^/]+)\/cancel$/);
    if (cancelMatch && method === 'POST') {
      cancelled.add(cancelMatch[1]);
      return { ok: true, status: 200, json: async () => ({}) } as Response;
    }
    const detailMatch = url.match(/\/sessions\/([^/]+)$/);
    if (detailMatch) {
      const found = opts.sessions.find((s) => s.id === detailMatch[1]);
      if (!found) return { ok: false, status: 404, json: async () => ({}) } as Response;
      return {
        ok: true,
        status: 200,
        json: async () => ({ ...found, status: cancelled.has(found.id) ? 'cancelled' : found.status }),
      } as Response;
    }
    throw new Error(`unexpected fetch: ${method} ${url}`);
  });
  return fetchMock;
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

describe('WorkstreamActivity phases', () => {
  it('renders "daemon unreachable" when the daemon cannot be reached', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 503, json: async () => ({}) }));
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('daemon unreachable') ?? false);
  });

  it('issue #3384: empty state reads "no active workstream — pick a project to start" (verbatim), never "session"', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({ activeWorkstreamId: null, workstreams: [], sessions: [] }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('no active workstream') ?? false);
    expect(target.textContent).toContain('no active workstream — pick a project to start');
    expect(target.textContent).not.toContain('no active session');
  });

  it('header reads "workstream activity", never "session monitor"', async () => {
    vi.stubGlobal('fetch', fakeDaemon({ activeWorkstreamId: null, workstreams: [], sessions: [] }));
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.querySelector('h2') !== null);

    expect(target.querySelector('h2')?.textContent?.trim()).toBe('workstream activity');
  });

  it('a real active workstream with no bound sessions yet renders its own "no activity yet" sub-state', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'acme-api — 2026-07-20', [])],
        sessions: [],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('no activity yet') ?? false);
    expect(target.textContent).toContain('acme-api — 2026-07-20');
    expect(target.textContent).toContain('no activity yet');
  });
});

describe('WorkstreamActivity workstream-scoped session selection', () => {
  it('never shows a session bound to a DIFFERENT workstream, even when it is the most recent overall', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'my workstream', ['bound-session'])],
        sessions: [
          // Newer, but NOT bound to ws-1 — must never be shown.
          session('unrelated-newer-session', 'running', '2026-07-20T15:00:00Z', 'someone else\'s task'),
          session('bound-session', 'running', '2026-07-20T14:00:00Z', 'my actual task'),
        ],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('my actual task') ?? false);
    expect(target.textContent).toContain('my actual task');
    expect(target.textContent).not.toContain("someone else's task");
  });

  it('shows the bound session\'s status/task/transcript and allows cancel', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'my workstream', ['bound-session'])],
        sessions: [session('bound-session', 'running', '2026-07-20T14:00:00Z', 'ship the feature')],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('ship the feature') ?? false);
    expect(target.textContent).toContain('running');

    const cancelButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'cancel',
    ) as HTMLButtonElement;
    expect(cancelButton).toBeTruthy();
    cancelButton.click();

    await waitFor(() => target.textContent?.includes('confirm cancel?') ?? false);
    const confirmButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'confirm cancel',
    ) as HTMLButtonElement;
    confirmButton.click();

    await waitFor(() => target.textContent?.includes('cancelled') ?? false);
  });
});

describe('WorkstreamActivity transcript-404 reset (PR #3028 regression, carried over)', () => {
  it('resets to the no-activity sub-state when the transcript fetch 404s after a successful detail fetch', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'my workstream', ['vanishing-session'])],
        sessions: [session('vanishing-session', 'running', '2026-07-20T14:00:00Z')],
        transcript404For: 'vanishing-session',
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('no activity yet') ?? false);
    // Must not be stuck showing the raw HTTP error or a stale Cancel button.
    expect(target.textContent).not.toContain('HTTP 404');
    expect(target.querySelectorAll('button').length).toBe(0);
  });
});
