// Why: `WorkstreamSwitcher.svelte` is the Phase C GUI switcher (DOC-48 §8,
// issue #3300) filling the header slot `AppHeader.svelte`/PR #3301 reserved.
// This covers every phase and interaction the component's own module docs
// describe: state display, the dropdown list with an active indicator,
// activation (including the `409` conflict banner), rename, and close —
// mirroring `SessionMonitor.test.ts`'s "mount the real component, stub
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
