// Why: DOC-39 §8.1 calls `.statusbar` nesting inside `.body` a regression
// that has "bit us twice in the wireframe" and explicitly asks for a
// DOM/structural test (AC-18.1) — this is that test, pinned at the shell
// level so a future refactor of `App.svelte` cannot silently reintroduce
// the nesting. The remaining assertions were rewritten for the issue #3153
// shell rebuild: `App.svelte` no longer stacks every card in one flat
// `.body` — content now lives behind `ServiceNav`'s 7 tabs, with Workstream
// (hosting `StartWorkingForm`/`WorkstreamActivity`, via `WorkstreamTab.svelte`)
// as the default `activeTab`, and `SearchTab` reachable only after
// selecting the Search nav button. These assertions exercise that through
// the same "mount the real component" approach as before, just following
// the new default-tab / tab-switch shape rather than assuming simultaneous
// rendering. Renamed twice since: issue #3365 `CreateSessionForm` ->
// `NewWorkstreamForm` (workstream-first entry), then issue #3384
// `NewWorkstreamForm` -> `StartWorkingForm` (explicit creation ceremony
// removed) + `SessionMonitor` -> `WorkstreamActivity` (reframed around the
// active workstream) — the assertions below follow both renames.
// What: Mounts the real `App.svelte` (with `fetch` stubbed so the mount
// doesn't make a real network call) and asserts `.statusbar` is a direct
// child of `.app` and NOT a descendant of `.body`, that the Workstream
// tab's content (`WorkstreamActivity`/`StartWorkingForm`) renders inside
// `.body` by default, and that clicking the Search nav tab swaps in
// `SearchTab`'s content.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import App from './App.svelte';

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
    vi.fn().mockResolvedValue({ ok: false, status: 503, json: async () => ({}) }),
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

describe('App shell structure (DOC-39 §8.1, AC-18.1)', () => {
  it('.statusbar is a sibling of .body, never a descendant', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const app = target.querySelector('.app');
    const body = target.querySelector('.body');
    const statusbar = target.querySelector('.statusbar');

    expect(app).not.toBeNull();
    expect(body).not.toBeNull();
    expect(statusbar).not.toBeNull();
    expect(statusbar!.parentElement).toBe(app);
    expect(body!.contains(statusbar)).toBe(false);
  });

  it('.hdr renders as .app\'s first child, .wsrail + .actpane inside .body (DOC-39 §8 skeleton)', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const app = target.querySelector('.app');
    const hdr = target.querySelector('.hdr');
    const body = target.querySelector('.body');
    const wsrail = target.querySelector('.wsrail');
    const actpane = target.querySelector('.actpane');
    const wsnav = target.querySelector('.wsnav');
    const actbody = target.querySelector('.actbody');

    expect(hdr).not.toBeNull();
    expect(hdr!.parentElement).toBe(app);
    expect(body!.contains(wsrail)).toBe(true);
    expect(body!.contains(actpane)).toBe(true);
    expect(actpane!.contains(wsnav)).toBe(true);
    expect(actpane!.contains(actbody)).toBe(true);
  });
});

describe('Workstream tab (default activeTab) — DOC-39 §4.6/§4.2.1', () => {
  it('WorkstreamActivity (8b card) renders inside .body by default', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const body = target.querySelector('.body');
    const headings = Array.from(body?.querySelectorAll('h2') ?? []);

    expect(body).not.toBeNull();
    expect(headings.some((h) => h.textContent === 'workstream activity')).toBe(true);
  });

  it('StartWorkingForm (issue #3384 — no explicit creation ceremony) renders inside .body by default', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const body = target.querySelector('.body');
    const headings = Array.from(body?.querySelectorAll('h2') ?? []);

    expect(body).not.toBeNull();
    expect(headings.some((h) => h.textContent === 'start working')).toBe(true);
  });
});

describe('ServiceNav tab switching (issue #3153 shell rebuild)', () => {
  it('SearchTab (10d) renders inside .body once a project-bound session unlocks the Search nav tab and it is selected', async () => {
    // Search is a project-scoped tab (AC-4.2: locked, not hidden, when
    // projectless) — unlike the default-mount tests above, this one needs
    // an active session carrying a `project` binding so the nav actually
    // unlocks the tab before the click can do anything.
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
                {
                  id: 'sess-1',
                  status: 'created',
                  created_at: new Date().toISOString(),
                  project: '/home/bob/acme-api',
                },
              ],
            }),
          } as Response;
        }
        return { ok: false, status: 404, json: async () => ({}) } as Response;
      }),
    );

    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const wsnav = target.querySelector('.wsnav');
    await waitFor(() => {
      const searchButton = Array.from(wsnav?.querySelectorAll('button') ?? []).find(
        (b) => b.textContent?.trim() === 'Search',
      ) as HTMLButtonElement | undefined;
      return searchButton !== undefined && !searchButton.disabled;
    });

    const searchButton = Array.from(wsnav?.querySelectorAll('button') ?? []).find(
      (b) => b.textContent?.trim() === 'Search',
    ) as HTMLButtonElement;
    searchButton.click();

    await waitFor(() => {
      const headings = Array.from(target.querySelector('.body')?.querySelectorAll('h2') ?? []);
      return headings.some((h) => h.textContent === 'search');
    });
  });

  it('renders all 7 canonical nav tabs', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const wsnav = target.querySelector('.wsnav');
    const labels = Array.from(wsnav?.querySelectorAll('button') ?? []).map((b) => b.textContent?.trim());

    for (const label of ['Workstream', 'Project', 'Agents', 'Memory', 'Search', 'Files']) {
      expect(labels, `expected a "${label}" nav tab`).toContain(label);
    }
  });

  it('Workflow tab is hidden (not just locked) when projectless (AC-9.4)', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const wsnav = target.querySelector('.wsnav');
    const labels = Array.from(wsnav?.querySelectorAll('button') ?? []).map((b) => b.textContent?.trim());

    expect(labels).not.toContain('Workflow');
  });

  it('project-scoped tabs (e.g. Project) render locked (disabled), not hidden, when projectless (AC-4.2)', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const wsnav = target.querySelector('.wsnav');
    const projectButton = Array.from(wsnav?.querySelectorAll('button') ?? []).find(
      (b) => b.textContent?.trim() === 'Project',
    ) as HTMLButtonElement | undefined;

    expect(projectButton).toBeTruthy();
    expect(projectButton!.disabled).toBe(true);
  });
});
