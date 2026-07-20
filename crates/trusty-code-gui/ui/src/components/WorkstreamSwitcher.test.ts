// Why: `WorkstreamSwitcher.svelte` is the Phase C GUI switcher (DOC-48 §8,
// issue #3300) filling the header slot `AppHeader.svelte`/PR #3301 reserved.
// This covers every phase and interaction the component's own module docs
// describe: state display, the dropdown list with an active indicator,
// activation (including the `409` conflict banner), rename, and close —
// mirroring `WorkstreamActivity.test.ts`'s "mount the real component, stub
// fetch, `waitFor` the settled DOM" approach, since this component shares
// its polling shape.
// What: A small in-memory fake daemon (`fakeDaemon`) backs a stubbed global
// `fetch`, so activate/rename/close calls actually mutate what the next
// `GET /workstreams` poll returns — real state transitions, not just "was
// the right URL called".
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import WorkstreamSwitcher from './WorkstreamSwitcher.svelte';
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

function ws(id: string, name: string, state: Workstream['state']): Workstream {
  return {
    id,
    name,
    state,
    session_ids: [],
    created_at: '2026-07-19T14:00:00Z',
    updated_at: '2026-07-19T14:00:00Z',
    metadata: {},
  };
}

/** A minimal in-memory fake of the `/workstreams*` REST surface. */
function fakeDaemon(initial: Workstream[], activeId: string | null = null) {
  const records = new Map(initial.map((w) => [w.id, { ...w }]));
  let active = activeId;
  let conflictOnNextActivate = false;

  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? 'GET';

    if (url.endsWith('/workstreams') && method === 'GET') {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          active_workstream_id: active,
          // `fetchWorkstreams` never sends `include_closed` -> the real
          // server's `workstream.list` default (`false`) applies, so a
          // closed record must drop out of this response, matching
          // `crate::workstreams::protocol::list`'s filter.
          workstreams: Array.from(records.values())
            .map((r) => ({
              ...r,
              state: r.id === active ? 'active' : r.state === 'closed' ? 'closed' : 'idle',
            }))
            .filter((r) => r.state !== 'closed'),
        }),
      } as Response;
    }

    const activateMatch = url.match(/\/workstreams\/([^/]+)\/activate$/);
    if (activateMatch && method === 'POST') {
      if (conflictOnNextActivate) {
        conflictOnNextActivate = false;
        return { ok: false, status: 409, json: async () => ({}) } as Response;
      }
      active = activateMatch[1];
      return { ok: true, status: 200, json: async () => ({ active_id: active, prior_id: null }) } as Response;
    }

    const closeMatch = url.match(/\/workstreams\/([^/]+)\/close$/);
    if (closeMatch && method === 'POST') {
      const record = records.get(closeMatch[1]);
      if (record) record.state = 'closed';
      if (active === closeMatch[1]) active = null;
      return { ok: true, status: 200, json: async () => ({}) } as Response;
    }

    const renameMatch = url.match(/\/workstreams\/([^/]+)\/rename$/);
    if (renameMatch && method === 'POST') {
      const record = records.get(renameMatch[1]);
      const body = JSON.parse(String(init?.body ?? '{}')) as { name: string };
      if (record) record.name = body.name;
      return { ok: true, status: 200, json: async () => record } as Response;
    }

    throw new Error(`unexpected fetch: ${method} ${url}`);
  });

  return {
    fetchMock,
    forceConflictOnNextActivate: () => {
      conflictOnNextActivate = true;
    },
  };
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

function trigger(): HTMLButtonElement {
  return target.querySelector('.wsswitch-trigger') as HTMLButtonElement;
}

describe('WorkstreamSwitcher phases', () => {
  it('renders "daemon unreachable" when the daemon cannot be reached', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 503 }));
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => trigger().textContent?.includes('daemon unreachable') ?? false);
    expect(trigger().disabled).toBe(true);
  });

  it('renders "no workstreams yet" and a disabled trigger when the daemon has none', async () => {
    const { fetchMock } = fakeDaemon([]);
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => trigger().textContent?.includes('no workstreams yet') ?? false);
    expect(trigger().disabled).toBe(true);
  });

  it('shows the active workstream\'s name in the trigger', async () => {
    const { fetchMock } = fakeDaemon(
      [ws('ws-1', 'Token rotation', 'idle'), ws('ws-2', 'Schema migration', 'idle')],
      'ws-1',
    );
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => trigger().textContent?.includes('Token rotation') ?? false);
    expect(trigger().disabled).toBe(false);
  });
});

describe('WorkstreamSwitcher dropdown + activation', () => {
  it('lists every workstream with an active indicator and activates a clicked row', async () => {
    const { fetchMock } = fakeDaemon(
      [ws('ws-1', 'Token rotation', 'idle'), ws('ws-2', 'Schema migration', 'idle')],
      'ws-1',
    );
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => trigger().textContent?.includes('Token rotation') ?? false);

    trigger().click();
    flushSync();

    const panel = target.querySelector('.wsswitch-panel') as HTMLElement;
    expect(panel).toBeTruthy();
    expect(panel.textContent).toContain('Token rotation');
    expect(panel.textContent).toContain('Schema migration');
    expect(panel.textContent).toContain('active');

    const rows = Array.from(panel.querySelectorAll('.wsswitch-row'));
    const schemaRow = rows.find((r) => r.textContent?.includes('Schema migration'));
    const activateButton = schemaRow?.querySelector('button') as HTMLButtonElement;
    activateButton.click();

    await waitFor(() => trigger().textContent?.includes('Schema migration') ?? false);
  });

  it('shows a conflict banner with a Refresh action on a 409, without auto-forcing', async () => {
    const daemon = fakeDaemon(
      [ws('ws-1', 'Token rotation', 'idle'), ws('ws-2', 'Schema migration', 'idle')],
      'ws-1',
    );
    daemon.forceConflictOnNextActivate();
    vi.stubGlobal('fetch', daemon.fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => trigger().textContent?.includes('Token rotation') ?? false);

    trigger().click();
    flushSync();
    const panel = target.querySelector('.wsswitch-panel') as HTMLElement;
    const rows = Array.from(panel.querySelectorAll('.wsswitch-row'));
    const schemaRow = rows.find((r) => r.textContent?.includes('Schema migration'));
    (schemaRow?.querySelector('button') as HTMLButtonElement).click();

    await waitFor(() => target.textContent?.includes('refresh to see the current one') ?? false);
    // Still shows the original active workstream — no silent force-switch.
    expect(trigger().textContent).toContain('Token rotation');

    const refreshButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'refresh',
    ) as HTMLButtonElement;
    refreshButton.click();
    await waitFor(() => !target.textContent?.includes('refresh to see the current one'));
  });
});

describe('WorkstreamSwitcher rename', () => {
  it('renames a workstream via the inline edit control', async () => {
    const { fetchMock } = fakeDaemon([ws('ws-1', 'Token rotation', 'idle')], 'ws-1');
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => trigger().textContent?.includes('Token rotation') ?? false);

    trigger().click();
    flushSync();
    const renameButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.getAttribute('aria-label') === 'rename Token rotation',
    ) as HTMLButtonElement;
    renameButton.click();
    flushSync();

    const input = target.querySelector('.wsswitch-panel input') as HTMLInputElement;
    expect(input).toBeTruthy();
    input.value = 'Token rotation v2';
    input.dispatchEvent(new Event('input'));
    flushSync();

    const saveButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'save',
    ) as HTMLButtonElement;
    saveButton.click();

    await waitFor(() => trigger().textContent?.includes('Token rotation v2') ?? false);
  });
});

describe('WorkstreamSwitcher close', () => {
  it('requires a two-step confirm before closing (no window.confirm)', async () => {
    const { fetchMock } = fakeDaemon(
      [ws('ws-1', 'Token rotation', 'idle'), ws('ws-2', 'Schema migration', 'idle')],
      'ws-1',
    );
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => trigger().textContent?.includes('Token rotation') ?? false);

    trigger().click();
    flushSync();
    const closeButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.getAttribute('aria-label') === 'close Schema migration',
    ) as HTMLButtonElement;
    closeButton.click();
    flushSync();

    expect(target.textContent).toContain('confirm');
    expect(target.textContent).toContain('never mind');

    const confirmButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'confirm',
    ) as HTMLButtonElement;
    confirmButton.click();

    // The panel stays open across the post-close `refresh()` — a closed
    // workstream drops out of the next `GET /workstreams` response (server
    // default `include_closed=false`), so its row must disappear from the
    // still-open panel without the operator re-toggling anything.
    await waitFor(
      () => !(target.querySelector('.wsswitch-panel') as HTMLElement)?.textContent?.includes('Schema migration'),
    );
    expect(target.querySelector('.wsswitch-panel')?.textContent).toContain('Token rotation');
  });
});

// code-critic PR #3356 review, HIGH: `renameId`/`closeConfirmId` were only
// cleared in `toggleOpen()`'s "closing" branch, so dismissing via the
// backdrop (a DIFFERENT code path) — or a successful activation — left them
// armed, and reopening never reset them. Repro: open -> arm close on row B
// -> backdrop-dismiss -> reopen -> row B must show its normal icons again,
// not a pre-armed "confirm / never mind".
describe('WorkstreamSwitcher backdrop-dismiss resets armed row state (code-critic HIGH)', () => {
  it('clears an armed close-confirm on backdrop-dismiss, so reopening shows normal icons', async () => {
    const { fetchMock } = fakeDaemon(
      [ws('ws-1', 'Token rotation', 'idle'), ws('ws-2', 'Schema migration', 'idle')],
      'ws-1',
    );
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => trigger().textContent?.includes('Token rotation') ?? false);

    trigger().click();
    flushSync();
    const armCloseButton = Array.from(target.querySelectorAll('button')).find(
      (b) => b.getAttribute('aria-label') === 'close Schema migration',
    ) as HTMLButtonElement;
    armCloseButton.click();
    flushSync();
    expect(target.textContent).toContain('never mind');

    // Dismiss via the BACKDROP, not the trigger — the code path the HIGH
    // finding says was missing the reset.
    const backdrop = target.querySelector(
      '[aria-label="close workstream switcher"]',
    ) as HTMLButtonElement;
    backdrop.click();
    flushSync();
    expect(target.querySelector('.wsswitch-panel')).toBeNull();

    trigger().click();
    flushSync();
    const panel = target.querySelector('.wsswitch-panel') as HTMLElement;
    expect(panel.textContent).not.toContain('never mind');
    const closeButtonAfterReopen = Array.from(panel.querySelectorAll('button')).find(
      (b) => b.getAttribute('aria-label') === 'close Schema migration',
    );
    expect(closeButtonAfterReopen).toBeTruthy();
  });
});

// code-critic PR #3356 review, MEDIUM 1: the poll interval and an
// SSE-triggered refresh share one `AbortController`, so neither aborts the
// other — a slow EARLIER call can resolve AFTER a fresher LATER one and
// overwrite it. This pins the monotonic-sequence guard: an interval tick
// that resolves after a subsequent tick must not clobber the subsequent
// tick's already-committed state.
describe('WorkstreamSwitcher out-of-order refresh guard (code-critic MEDIUM 1)', () => {
  function deferredResponse(): {
    promise: Promise<Response>;
    resolve: (r: Response) => void;
  } {
    let resolve!: (r: Response) => void;
    const promise = new Promise<Response>((res) => {
      resolve = res;
    });
    return { promise, resolve };
  }

  it('a slow, earlier poll response must not overwrite a fresher, later one', async () => {
    vi.useFakeTimers();
    try {
      const stale = deferredResponse();
      const fresh = deferredResponse();
      let call = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (!url.endsWith('/workstreams')) throw new Error(`unexpected fetch: ${url}`);
        call += 1;
        return call === 1 ? stale.promise : fresh.promise;
      });
      vi.stubGlobal('fetch', fetchMock);

      instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;

      // Flush the initial mount `$effect`: issues the FIRST request, which
      // stays pending (not yet resolved).
      await vi.advanceTimersByTimeAsync(0);
      expect(call).toBe(1);
      expect(trigger().textContent).toContain('connecting');

      // Advance past POLL_MS (5000ms) to fire the interval's SECOND request
      // while the first is still in flight.
      await vi.advanceTimersByTimeAsync(5000);
      expect(call).toBe(2);

      // The SECOND (later-started) request resolves FIRST, with fresh data.
      fresh.resolve({
        ok: true,
        status: 200,
        json: async () => ({
          active_workstream_id: 'ws-2',
          workstreams: [ws('ws-2', 'Fresh workstream', 'active')],
        }),
      } as Response);
      await vi.advanceTimersByTimeAsync(0);
      expect(trigger().textContent).toContain('Fresh workstream');

      // The FIRST (earlier-started) request finally resolves, late, with
      // stale data — it must NOT clobber the fresher state already shown.
      stale.resolve({
        ok: true,
        status: 200,
        json: async () => ({
          active_workstream_id: 'ws-1',
          workstreams: [ws('ws-1', 'Stale workstream', 'active')],
        }),
      } as Response);
      await vi.advanceTimersByTimeAsync(0);

      expect(trigger().textContent).toContain('Fresh workstream');
      expect(trigger().textContent).not.toContain('Stale workstream');
    } finally {
      vi.useRealTimers();
    }
  });
});

// code-critic PR #3392 review, HIGH: the SSE-subscription `$effect` used to
// read `list?.active_workstream_id` directly. `refresh()` reassigns `list`
// to a FRESHLY-PARSED object on every poll tick even when the active id is
// unchanged — Svelte 5 invalidates a dependent on reference inequality of
// the `$state` value read, so the effect re-ran (closing + reopening the
// `EventSource`) on EVERY poll tick, not only when the active id actually
// changed. Fixed by routing the effect through `activeWorkstreamId`, a
// `$derived` PRIMITIVE (Svelte 5 suppresses a dependent's re-run when a
// `$derived` recomputes to the SAME primitive value). jsdom has no real
// `EventSource`, so this stubs a minimal fake on `globalThis` and counts
// constructions across several same-active-id poll ticks.
describe('WorkstreamSwitcher SSE subscription reactivity (code-critic PR #3392 review, HIGH)', () => {
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
      const { fetchMock } = fakeDaemon([ws('ws-stable', 'Token rotation', 'idle')], 'ws-stable');
      vi.stubGlobal('fetch', fetchMock);

      instance = mount(WorkstreamSwitcher, { target }) as unknown as Record<string, unknown>;

      // Flush the initial mount tick and let the SSE effect's own async IIFE
      // (apiBase() -> new EventSource()) settle.
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(0);
      expect(FakeEventSource.instances.length).toBe(1);

      // Three more poll ticks (POLL_MS = 5000), each returning a BRAND NEW
      // `list` object with the identical active id — the exact
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
