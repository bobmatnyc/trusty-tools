// Why: PR #3301 critic review (MEDIUM) — `WorkstreamRail.svelte`'s doc
// comment previously claimed "no dedicated component test yet... no
// pure/branchy logic to isolate", but the component has four real
// branches: the collapsed/expanded width class, the `railView` segmented
// toggle swapping list content, the Workstream view's
// active-session-present-vs-absent rendering, and (as of this same review
// round) the "+ add project" button actually invoking
// `onSwitchToWorkstream`. This file covers all four, matching the rigor
// `ServiceNav`/`nav-tabs.test.ts` already apply to their sibling shell
// components.
// What: Mounts the real component with a stub `onToggleCollapse`/
// `onSwitchToWorkstream`, toggles props/state via re-mounting (props are
// not reactively bindable from outside a test the way a parent component
// would rebind them, so each case mounts with the props it needs), and
// asserts on the rendered DOM.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import WorkstreamRail from './WorkstreamRail.svelte';
import type { SessionSummary } from '../lib/session-status';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

const SESSION: SessionSummary = {
  id: 'sess-1',
  status: 'running',
  created_at: new Date().toISOString(),
  project: '/home/bob/acme-api',
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
});

function aside(): HTMLElement {
  return target.querySelector('.wsrail') as HTMLElement;
}

describe('WorkstreamRail collapse toggle', () => {
  it('renders the expanded width class (w-60) when collapsed=false', () => {
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
  it('defaults to the workstream list and swaps to the project list on click', () => {
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: false, onToggleCollapse: () => {}, onSwitchToWorkstream: () => {} },
    }) as unknown as Record<string, unknown>;

    // Default view: workstream list (no active session -> the empty-state line).
    expect(aside().textContent).toContain('no active workstream yet');
    expect(aside().textContent).not.toContain('add project');

    const projectsButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'projects',
    ) as HTMLButtonElement;
    expect(projectsButton).toBeTruthy();
    projectsButton.click();
    flushSync();

    expect(aside().textContent).toContain('add project');
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

describe('WorkstreamRail "+ add project" (issue #3153 PR #3301 review, MEDIUM)', () => {
  it('invokes onSwitchToWorkstream when clicked', () => {
    const onSwitchToWorkstream = vi.fn();
    instance = mount(WorkstreamRail, {
      target,
      props: { collapsed: false, onToggleCollapse: () => {}, onSwitchToWorkstream },
    }) as unknown as Record<string, unknown>;

    const projectsButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'projects',
    ) as HTMLButtonElement;
    projectsButton.click();
    flushSync();

    const addProjectButton = Array.from(aside().querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === '+ add project',
    ) as HTMLButtonElement;
    expect(addProjectButton).toBeTruthy();
    addProjectButton.click();

    expect(onSwitchToWorkstream).toHaveBeenCalledOnce();
  });
});
