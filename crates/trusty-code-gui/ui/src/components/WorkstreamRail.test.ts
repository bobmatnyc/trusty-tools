// Why: PR #3301 critic review (MEDIUM) — `WorkstreamRail.svelte`'s doc
// comment previously claimed "no dedicated component test yet... no
// pure/branchy logic to isolate", but the component has real branches: the
// collapsed/expanded width class, the `railView` segmented toggle swapping
// list content, the Workstream view's active-session-present-vs-absent
// rendering, and (issue #3447) the Project view's real roster fetch/
// render/phase/selection behavior. This file covers all of that, matching
// the rigor `ServiceNav`/`nav-tabs.test.ts` already apply to their sibling
// shell components.
// What: Mounts the real component with a stub `onToggleCollapse`/
// `onSwitchToWorkstream` and `fetch` stubbed for `GET /projects` (mirroring
// `ProjectPickerModal.test.ts`'s pattern for the identical route), toggles
// props/state via re-mounting (props are not reactively bindable from
// outside a test the way a parent component would rebind them, so each case
// mounts with the props it needs), and asserts on the rendered DOM. Resets
// the shared `selectedProjectState` store between tests (module-level state
// persists across tests in the same file otherwise).
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import WorkstreamRail from './WorkstreamRail.svelte';
import { selectProject } from '../lib/selected-project.svelte';
import type { SessionSummary } from '../lib/session-status';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

const SESSION: SessionSummary = {
  id: 'sess-1',
  status: 'running',
  created_at: new Date().toISOString(),
  project: '/home/bob/acme-api',
};

const ROSTER = {
  entries: [
    {
      name: 'trusty-tools',
      path: '/home/bob/trusty-mpm-projects/bobmatnyc/trusty-tools',
      owner: 'bobmatnyc',
      registered: true,
    },
    { name: 'acme-api', path: '/home/bob/acme-api', owner: null, registered: true },
  ],
  source: 'registry',
};

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('timed out waiting for condition');
}

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
  selectProject(null);
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  selectProject(null);
  vi.unstubAllGlobals();
});

function aside(): HTMLElement {
  return target.querySelector('.wsrail') as HTMLElement;
}

function stubRosterFetch(body: unknown = ROSTER) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/projects')) {
        return { ok: true, status: 200, json: async () => body } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    }),
  );
}

describe('WorkstreamRail collapse toggle', () => {
  it('renders the expanded width class (w-60) when collapsed=false', () => {
    stubRosterFetch();
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: false, onToggleCollapse: () => {}, onSwitchToWorkstream: () => {} },
    }) as unknown as Record<string, unknown>;

    expect(aside().className).toContain('w-60');
    expect(aside().className).not.toContain('w-[46px]');
  });

  it('renders the collapsed width class (w-[46px]) when collapsed=true, and hides the toggle/list', () => {
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: true, onToggleCollapse: () => {}, onSwitchToWorkstream: () => {} },
    }) as unknown as Record<string, unknown>;

    expect(aside().className).toContain('w-[46px]');
    expect(aside().className).not.toContain('w-60');
    expect(target.querySelector('.wsrail')?.textContent).not.toContain('workstreams');
  });

  it('calls onToggleCollapse when the «/» button is clicked', () => {
    const onToggleCollapse = vi.fn();
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: false, onToggleCollapse, onSwitchToWorkstream: () => {} },
    }) as unknown as Record<string, unknown>;

    const toggleButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent === '«',
    ) as HTMLButtonElement;
    expect(toggleButton).toBeTruthy();
    toggleButton.click();
    expect(onToggleCollapse).toHaveBeenCalledOnce();
  });
});

describe('WorkstreamRail railView segmented toggle', () => {
  it('defaults to the workstream list and swaps to the project list on click', async () => {
    stubRosterFetch();
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: false, onToggleCollapse: () => {}, onSwitchToWorkstream: () => {} },
    }) as unknown as Record<string, unknown>;

    // Default view: workstream list (no active session -> the empty-state line).
    expect(aside().textContent).toContain('no active workstream yet');
    expect(aside().textContent).not.toContain('acme-api');

    const projectsButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'projects',
    ) as HTMLButtonElement;
    expect(projectsButton).toBeTruthy();
    projectsButton.click();
    flushSync();

    await waitFor(() => aside().textContent?.includes('acme-api') ?? false);
    expect(aside().textContent).not.toContain('no active workstream yet');
  });
});

describe('WorkstreamRail active-session rendering (workstream view)', () => {
  it('renders the empty-state note when there is no active session', () => {
    instance = mount(WorkstreamRail, {
      target,
      props: {
        collapsed: false,
        onToggleCollapse: () => {},
        onSwitchToWorkstream: () => {},
        activeSession: null,
      },
    }) as unknown as Record<string, unknown>;

    expect(aside().textContent).toContain('no active workstream yet');
  });

  it('renders the synthetic current-session card when a session is active', () => {
    instance = mount(WorkstreamRail, {
      target,
      props: {
        collapsed: false,
        onToggleCollapse: () => {},
        onSwitchToWorkstream: () => {},
        activeSession: SESSION,
      },
    }) as unknown as Record<string, unknown>;

    expect(aside().textContent).toContain(SESSION.status);
    expect(aside().textContent).toContain(SESSION.project as string);
    expect(aside().textContent).not.toContain('no active workstream yet');
  });
});

describe('WorkstreamRail Project view (issue #3447 — rail is the primary project picker)', () => {
  function openProjectView(onSwitchToWorkstream: () => void = () => {}) {
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: false, onToggleCollapse: () => {}, onSwitchToWorkstream },
    }) as unknown as Record<string, unknown>;
    const projectsButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'projects',
    ) as HTMLButtonElement;
    projectsButton.click();
    flushSync();
  }

  it('fetches and renders the roster', async () => {
    stubRosterFetch();
    openProjectView();
    await waitFor(() => aside().textContent?.includes('acme-api') ?? false);
    expect(aside().textContent).toContain('bobmatnyc/trusty-tools');
    expect(aside().textContent).toContain('acme-api');
  });

  it('marks an unregistered (local-only) entry, not a registered one', async () => {
    stubRosterFetch({
      entries: [
        { name: 'trusty-tools', path: '/home/bob/trusty-tools', owner: 'bobmatnyc', registered: true },
        { name: 'bakeoff-l1', path: '/home/bob/bakeoff-l1', owner: 'bobmatnyc', registered: false },
      ],
    });
    openProjectView();
    await waitFor(() => aside().textContent?.includes('bakeoff-l1') ?? false);

    const rows = Array.from(aside().querySelectorAll('button'));
    const registeredRow = rows.find((b) => b.textContent?.includes('bobmatnyc/trusty-tools'));
    const unregisteredRow = rows.find((b) => b.textContent?.includes('bakeoff-l1'));
    expect(registeredRow?.textContent).not.toContain('local only');
    expect(unregisteredRow?.textContent).toContain('local only');
  });

  it('shows the fs_only banner only when the roster degraded', async () => {
    stubRosterFetch({
      entries: [{ name: 'bakeoff-l1', path: '/home/bob/bakeoff-l1', owner: null, registered: false }],
      source: 'fs_only',
    });
    openProjectView();
    await waitFor(() => aside().textContent?.includes('bakeoff-l1') ?? false);
    expect(aside().textContent).toContain('shared registry unavailable');
  });

  it('shows an empty-roster message distinct from loading/error', async () => {
    stubRosterFetch({ entries: [] });
    openProjectView();
    await waitFor(() => aside().textContent?.includes('no known projects found') ?? false);
  });

  it('shows daemon-unreachable when GET /projects fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('Failed to fetch');
      }),
    );
    openProjectView();
    await waitFor(() => aside().textContent?.includes('daemon unreachable') ?? false);
  });

  it('issue #3447: clicking a roster row selects it into the shared store and switches to the Workstream tab', async () => {
    const onSwitchToWorkstream = vi.fn();
    stubRosterFetch();
    openProjectView(onSwitchToWorkstream);
    await waitFor(() => aside().textContent?.includes('acme-api') ?? false);

    const row = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement;
    row.click();

    const { selectedProjectState } = await import('../lib/selected-project.svelte');
    expect(selectedProjectState.project).toEqual({
      path: '/home/bob/acme-api',
      displayPath: 'acme-api',
      isGitRepo: true,
    });
    expect(onSwitchToWorkstream).toHaveBeenCalledOnce();
  });

  it('issue #3447: the projectless row clears the shared selection and switches tabs', async () => {
    const onSwitchToWorkstream = vi.fn();
    stubRosterFetch();
    openProjectView(onSwitchToWorkstream);
    await waitFor(() => aside().textContent?.includes('acme-api') ?? false);

    const clearButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'projectless (chat only)',
    ) as HTMLButtonElement;
    clearButton.click();

    const { selectedProjectState } = await import('../lib/selected-project.svelte');
    expect(selectedProjectState.project).toBeNull();
    expect(onSwitchToWorkstream).toHaveBeenCalledOnce();
  });

  it('issue #3447: a roster row matching the shared selection is visually highlighted', async () => {
    selectProject({ path: '/home/bob/acme-api', displayPath: 'acme-api', isGitRepo: true });
    stubRosterFetch();
    openProjectView();
    await waitFor(() => aside().textContent?.includes('acme-api') ?? false);

    const row = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement;
    expect(row.className).toContain('bg-trusty-sidebar-active');
  });
});
