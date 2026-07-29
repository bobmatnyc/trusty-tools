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
// sub-empty-state, cancel's two-step confirm, the transcript-404-reset
// regression PR #3028's code-critic review originally pinned (still
// exercised here, just through the new two-poll chain), the SSE-subscription
// reactivity fix (code-critic PR #3392 review, HIGH), and the
// pending-workstream fallback (code-critic PR #3392 review, MEDIUM).
// What: A small in-memory fake daemon backs a stubbed global `fetch`,
// mirroring `WorkstreamSwitcher.test.ts`'s `fakeDaemon` shape — extended to
// also answer `GET /sessions`, `GET /sessions/{id}`,
// `GET /sessions/{id}/transcript`, and `POST /sessions/{id}/cancel`.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import WorkstreamActivity from './WorkstreamActivity.svelte';
import { activeWorkstreamState, setActiveWorkstreamId } from '../lib/active-workstream.svelte';
import { clearPendingWorkstream, setPendingWorkstream } from '../lib/pending-workstream.svelte';
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
  transcriptFor?: Record<string, Array<{ role: string; text: string; tool_calls?: string[] }>>;
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
    const markdownMatch = url.match(/\/sessions\/([^/]+)\/transcript\.md$/);
    if (markdownMatch) {
      // The daemon renders the transcript to Markdown (issue #3526); the fake
      // returns a recognizable canned document so the download-wiring test can
      // assert the bytes it fetched are what got saved.
      const md = `# Workstream transcript — ${markdownMatch[1]}\n\n- \`PYTHON-ENGINEER\` ran: write_files\n`;
      return {
        ok: true,
        status: 200,
        text: async () => md,
      } as unknown as Response;
    }
    const transcriptMatch = url.match(/\/sessions\/([^/]+)\/transcript$/);
    if (transcriptMatch) {
      if (transcriptMatch[1] === opts.transcript404For) {
        return { ok: false, status: 404, json: async () => ({}) } as Response;
      }
      const turns = opts.transcriptFor?.[transcriptMatch[1]] ?? [];
      return { ok: true, status: 200, json: async () => ({ turns }) } as Response;
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
  // The pending-workstream fallback marker and the shared active-workstream
  // id (code-critic PR #3460 review, HIGH 2 — this component is one of its
  // two writers) are MODULE-level stores shared across every test in this
  // file — reset them so one test's value never leaks into the next.
  clearPendingWorkstream();
  setActiveWorkstreamId(null);
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
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

  it('issue #3446: renders as a full-pane chat stream with no card chrome — no "session monitor" wording, no bounded header', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'acme-api — 2026-07-20', [])],
        sessions: [],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => target.textContent?.includes('acme-api — 2026-07-20') ?? false);

    // No h2 card-header chrome left — the pane fills its host directly.
    expect(target.querySelector('h2')).toBeNull();
    expect(target.textContent).not.toContain('session monitor');
    // The root fills its host's height (WorkstreamTab hosts it as flex-1).
    expect(target.querySelector('section')?.className).toContain('h-full');
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

describe('WorkstreamActivity pending-workstream fallback (code-critic PR #3392 review, MEDIUM)', () => {
  it('falls back to the pending (not-yet-active) workstream when the daemon reports no active pointer, instead of the empty state', async () => {
    setPendingWorkstream('ws-pending', 'ship the feature — 2026-07-20');
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        // No real active pointer — mirrors an activation that failed after a
        // successful task-run.
        activeWorkstreamId: null,
        workstreams: [ws('ws-pending', 'ship the feature — 2026-07-20', ['bound-session'])],
        sessions: [session('bound-session', 'running', '2026-07-20T14:00:00Z', 'ship the feature')],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('ship the feature') ?? false);
    expect(target.textContent).not.toContain('no active workstream — pick a project to start');
    expect(target.textContent).toContain('ship the feature — 2026-07-20');
    expect(target.textContent).toContain('running');
  });

  it('ignores a stale pending-workstream id that no longer exists in the current workstreams list', async () => {
    setPendingWorkstream('ws-vanished', 'a workstream that no longer exists');
    vi.stubGlobal(
      'fetch',
      fakeDaemon({ activeWorkstreamId: null, workstreams: [], sessions: [] }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('no active workstream') ?? false);
    expect(target.textContent).toContain('no active workstream — pick a project to start');
    expect(target.textContent).not.toContain('a workstream that no longer exists');
  });

  it('prefers the REAL active pointer over the pending marker when both are present', async () => {
    setPendingWorkstream('ws-pending', 'stale pending name');
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-real-active',
        workstreams: [
          ws('ws-pending', 'stale pending name', []),
          ws('ws-real-active', 'the actually active workstream', []),
        ],
        sessions: [],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('the actually active workstream') ?? false);
    expect(target.textContent).not.toContain('stale pending name');
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

describe('WorkstreamActivity publishes the resolved active id to the shared store (code-critic PR #3460 review, HIGH 2)', () => {
  it('writes the real active id after a poll', async () => {
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'my workstream', [])],
        sessions: [],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => activeWorkstreamState.id === 'ws-1');
  });

  it('writes the RESOLVED id (pending fallback) when the daemon reports no real pointer, and null when nothing resolves', async () => {
    setPendingWorkstream('ws-pending', 'pending name');
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: null,
        workstreams: [ws('ws-pending', 'pending name', [])],
        sessions: [],
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => activeWorkstreamState.id === 'ws-pending');

    // Remount against an empty daemon: nothing resolves -> null published.
    unmount(instance!);
    instance = null;
    clearPendingWorkstream();
    vi.stubGlobal('fetch', fakeDaemon({ activeWorkstreamId: null, workstreams: [], sessions: [] }));
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => activeWorkstreamState.id === null);
  });
});

describe('WorkstreamActivity chat stream (issue #3446)', () => {
  it('renders every turn, untruncated, oldest first (no more 5-line/160-char bound)', async () => {
    const longText = 'x'.repeat(500);
    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'my workstream', ['bound-session'])],
        sessions: [session('bound-session', 'running', '2026-07-20T14:00:00Z', 'ship the feature')],
        transcriptFor: {
          'bound-session': [
            { role: 'user', text: 'first message' },
            { role: 'assistant', text: '', tool_calls: ['grep', 'read_file'] },
            { role: 'assistant', text: longText },
          ],
        },
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes(longText) ?? false);
    expect(target.textContent).toContain('first message');
    expect(target.textContent).toContain('ran: grep, read_file');
    expect(target.textContent).toContain(longText);

    // Oldest-first order: "first message" must precede the tool-only turn,
    // which must precede the long final turn.
    const text = target.textContent ?? '';
    const iFirst = text.indexOf('first message');
    const iTool = text.indexOf('ran: grep, read_file');
    const iLong = text.indexOf(longText);
    expect(iFirst).toBeGreaterThanOrEqual(0);
    expect(iFirst).toBeLessThan(iTool);
    expect(iTool).toBeLessThan(iLong);
  });
});

describe('WorkstreamActivity download transcript (issue #3526)', () => {
  it('fetches the daemon-rendered Markdown and triggers a download of it', async () => {
    const createObjectURL = vi.fn(() => 'blob:mock-url');
    const revokeObjectURL = vi.fn();
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL } as unknown as typeof URL);

    let capturedBlob: Blob | undefined;
    let capturedDownload: string | undefined;
    const realCreate = document.createElement.bind(document);
    const createSpy = vi.spyOn(document, 'createElement').mockImplementation((tag: string) => {
      const el = realCreate(tag) as HTMLElement;
      if (tag === 'a') {
        // Capture the download filename and swallow the click so jsdom doesn't
        // attempt a real navigation.
        el.click = () => {
          capturedDownload = (el as HTMLAnchorElement).download;
        };
      }
      return el;
    });
    // Capture the Blob the component constructs, plus its Markdown payload —
    // jsdom's Blob doesn't implement `.text()`, so read the constructor parts
    // directly rather than round-tripping through the Blob.
    let capturedMarkdown = '';
    const RealBlob = globalThis.Blob;
    vi.stubGlobal(
      'Blob',
      class extends RealBlob {
        constructor(parts?: BlobPart[], options?: BlobPropertyBag) {
          super(parts, options);
          capturedBlob = this;
          capturedMarkdown = (parts ?? []).map((p) => String(p)).join('');
        }
      },
    );

    vi.stubGlobal(
      'fetch',
      fakeDaemon({
        activeWorkstreamId: 'ws-1',
        workstreams: [ws('ws-1', 'my workstream', ['bound-session'])],
        sessions: [session('bound-session', 'running', '2026-07-20T14:00:00Z', 'ship the feature')],
        transcriptFor: {
          'bound-session': [
            { role: 'pm', text: 'delegating' },
            { role: 'python-engineer', text: '', tool_calls: ['write_files'] },
          ],
        },
      }),
    );
    instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('delegating') ?? false);

    const downloadButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'download transcript',
    ) as HTMLButtonElement;
    expect(downloadButton).toBeTruthy();

    downloadButton.click();

    // The download awaits an async fetch of the `.md` endpoint before building
    // the Blob, so wait for the object-URL call rather than asserting inline.
    await waitFor(() => createObjectURL.mock.calls.length > 0);
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(revokeObjectURL).toHaveBeenCalledTimes(1);
    expect(capturedDownload).toMatch(/^transcript-ws-1-\d{8}-\d{6}\.md$/);
    expect(capturedBlob).toBeTruthy();
    // The saved bytes are exactly what the daemon endpoint returned.
    expect(capturedMarkdown).toContain('# Workstream transcript — bound-session');
    expect(capturedMarkdown).toContain('- `PYTHON-ENGINEER` ran: write_files');

    createSpy.mockRestore();
  });
});

describe('WorkstreamActivity auto-scroll with scroll-lock (issue #3446)', () => {
  function streamEl(): HTMLDivElement {
    return target.querySelector('.activity-stream') as HTMLDivElement;
  }

  /** jsdom hardcodes scrollHeight/clientHeight to 0 — stub them so the
   * component's `onStreamScroll`/auto-scroll-effect distance math has real
   * numbers to compare against. */
  function stubLayout(el: HTMLDivElement, opts: { scrollHeight: number; clientHeight: number }) {
    Object.defineProperty(el, 'scrollHeight', { value: opts.scrollHeight, configurable: true });
    Object.defineProperty(el, 'clientHeight', { value: opts.clientHeight, configurable: true });
  }

  it('scrolls to the bottom on new content when the operator has not scrolled up', async () => {
    vi.useFakeTimers();
    try {
      vi.stubGlobal(
        'fetch',
        fakeDaemon({
          activeWorkstreamId: 'ws-1',
          workstreams: [ws('ws-1', 'my workstream', ['bound-session'])],
          sessions: [session('bound-session', 'running', '2026-07-20T14:00:00Z')],
          transcriptFor: { 'bound-session': [{ role: 'user', text: 'hello' }] },
        }),
      );
      instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(0);
      expect(target.textContent).toContain('hello');

      const el = streamEl();
      stubLayout(el, { scrollHeight: 900, clientHeight: 200 });
      el.scrollTop = 0;

      // The next poll tick reassigns `transcript` to a freshly-parsed object
      // (same content, new reference), which recomputes `chatEntries` and
      // fires the auto-scroll effect — never scrolled up, so it pins to the
      // bottom.
      await vi.advanceTimersByTimeAsync(5000);
      await vi.advanceTimersByTimeAsync(0);
      expect(el.scrollTop).toBe(900);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not yank the view back down once the operator has scrolled up to read backlog', async () => {
    vi.useFakeTimers();
    try {
      vi.stubGlobal(
        'fetch',
        fakeDaemon({
          activeWorkstreamId: 'ws-1',
          workstreams: [ws('ws-1', 'my workstream', ['bound-session'])],
          sessions: [session('bound-session', 'running', '2026-07-20T14:00:00Z')],
          transcriptFor: { 'bound-session': [{ role: 'user', text: 'hello' }] },
        }),
      );
      instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(0);
      expect(target.textContent).toContain('hello');

      const el = streamEl();
      stubLayout(el, { scrollHeight: 900, clientHeight: 200 });
      // Operator scrolls up, far from the bottom (well past the component's
      // scroll-lock threshold).
      el.scrollTop = 100;
      el.dispatchEvent(new Event('scroll'));

      // A poll tick lands (transcript reassigned, same content) — scrollTop
      // must stay put, not get reset to the bottom underneath the operator.
      await vi.advanceTimersByTimeAsync(5000);
      await vi.advanceTimersByTimeAsync(0);
      expect(el.scrollTop).toBe(100);
    } finally {
      vi.useRealTimers();
    }
  });
});

// code-critic PR #3392 review, HIGH: the SSE-subscription `$effect` used to
// read `activeWorkstream?.id` directly. `refresh()` reassigns `activeWorkstream`
// to a FRESHLY-PARSED object on every poll tick even when the active id is
// unchanged — Svelte 5 invalidates a dependent on reference inequality of the
// `$state` value read, so the effect re-ran (closing + reopening the
// `EventSource`) on EVERY tick, not only when the id actually changed. Fixed
// by routing the effect through `activeWorkstreamId`, a `$derived` PRIMITIVE
// (Svelte 5 suppresses a dependent's re-run when a `$derived` recomputes to
// the SAME primitive value). jsdom has no real `EventSource`, so this stubs a
// minimal fake on `globalThis` and counts constructions across several
// same-active-id poll ticks (fake timers advance past POLL_MS repeatedly).
describe('WorkstreamActivity SSE subscription reactivity (code-critic PR #3392 review, HIGH)', () => {
  class FakeEventSource {
    static instances: FakeEventSource[] = [];
    url: string;
    onmessage: ((e: MessageEvent) => void) | null = null;
    constructor(url: string) {
      this.url = url;
      FakeEventSource.instances.push(this);
    }
    close() {
      /* no-op */
    }
  }

  it('does not reopen the EventSource on repeated poll ticks that report the SAME active workstream id', async () => {
    FakeEventSource.instances = [];
    vi.stubGlobal('EventSource', FakeEventSource);
    vi.useFakeTimers();
    try {
      vi.stubGlobal(
        'fetch',
        fakeDaemon({
          activeWorkstreamId: 'ws-stable',
          workstreams: [ws('ws-stable', 'my workstream', [])],
          sessions: [],
        }),
      );

      instance = mount(WorkstreamActivity, { target }) as unknown as Record<string, unknown>;

      // Flush the initial mount tick (poll #1) and let the SSE effect's own
      // async IIFE (apiBase() -> new EventSource()) settle.
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(0);
      expect(FakeEventSource.instances.length).toBe(1);

      // Three more poll ticks (POLL_MS = 5000), each returning a BRAND NEW
      // `activeWorkstream` object with the identical id — the exact
      // reassign-a-fresh-object-every-tick shape that triggered the bug.
      for (let i = 0; i < 3; i += 1) {
        await vi.advanceTimersByTimeAsync(5000);
        await vi.advanceTimersByTimeAsync(0);
      }

      expect(FakeEventSource.instances.length).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

// tcode streaming epic #3696, Slice 3: the session-scoped `GET
// /sessions/{id}/events` delta stream, gated behind `VITE_TCODE_SSE`
// (`vi.stubEnv` — Vitest reflects a stubbed env var onto `import.meta.env` the
// same way Vite's dev/build modes do, so this exercises the exact
// `SSE_ENABLED` check the component reads). Reuses the same
// stub-`globalThis.EventSource` technique as the reactivity describe block
// above, but this fake also exposes `emit` so the test can push a fabricated
// `SessionEventEnvelope` frame through `onmessage` the way a real SSE
// connection would deliver one.
describe('WorkstreamActivity live delta streaming (tcode streaming epic #3696, Slice 3)', () => {
  class FakeEventSource {
    static instances: FakeEventSource[] = [];
    url: string;
    onmessage: ((e: MessageEvent) => void) | null = null;
    constructor(url: string) {
      this.url = url;
      FakeEventSource.instances.push(this);
    }
    close() {
      /* no-op */
    }
    emit(data: unknown) {
      this.onmessage?.({ data: JSON.stringify(data) } as MessageEvent);
    }
  }

  function agentMessageDeltaEnvelope(seq: number, overrides: Record<string, unknown> = {}) {
    return {
      seq,
      event: {
        type: 'agent_message_delta',
        session_id: 'bound-session',
        agent: 'python-engineer',
        agent_id: 'agent-1',
        turn_id: 'turn-1',
        delta: '',
        done: false,
        ...overrides,
      },
    };
  }

  beforeEach(() => {
    FakeEventSource.instances = [];
  });

  it('stays poll-only (no session-scoped EventSource) when VITE_TCODE_SSE is unset (default OFF)', async () => {
    vi.stubGlobal('EventSource', FakeEventSource);
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
    expect(
      FakeEventSource.instances.some((s) => s.url.endsWith('/sessions/bound-session/events')),
    ).toBe(false);
  });

  it('renders incremental text as deltas arrive, then finalizes the bubble on done:true', async () => {
    vi.stubEnv('VITE_TCODE_SSE', 'true');
    vi.stubGlobal('EventSource', FakeEventSource);
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
    await waitFor(() =>
      FakeEventSource.instances.some((s) => s.url.endsWith('/sessions/bound-session/events')),
    );
    const sessionSource = FakeEventSource.instances.find((s) =>
      s.url.endsWith('/sessions/bound-session/events'),
    )!;

    sessionSource.emit(agentMessageDeltaEnvelope(1, { delta: 'Hel', done: false }));
    await waitFor(() => target.textContent?.includes('Hel') ?? false);
    // Incremental text visible BEFORE the final delta — not yet the full word.
    expect(target.textContent).not.toContain('Hello');
    expect(target.querySelector('[title="streaming…"]')).toBeTruthy();

    sessionSource.emit(agentMessageDeltaEnvelope(2, { delta: 'lo', done: true }));
    await waitFor(() => target.textContent?.includes('Hello') ?? false);
    // The bubble finalizes: streaming affordance removed once done:true lands.
    expect(target.querySelector('[title="streaming…"]')).toBeNull();
  });

  it('ignores envelopes that are not agent_message_delta and malformed frames, without throwing', async () => {
    vi.stubEnv('VITE_TCODE_SSE', 'true');
    vi.stubGlobal('EventSource', FakeEventSource);
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
    await waitFor(() =>
      FakeEventSource.instances.some((s) => s.url.endsWith('/sessions/bound-session/events')),
    );
    const sessionSource = FakeEventSource.instances.find((s) =>
      s.url.endsWith('/sessions/bound-session/events'),
    )!;

    expect(() => sessionSource.onmessage?.({ data: 'not json' } as MessageEvent)).not.toThrow();
    expect(() =>
      sessionSource.emit({ seq: 1, event: { type: 'tool_called', session_id: 'bound-session' } }),
    ).not.toThrow();
    expect(target.querySelector('[title="streaming…"]')).toBeNull();
  });
});
