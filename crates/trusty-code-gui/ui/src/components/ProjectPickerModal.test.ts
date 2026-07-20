// Why: `ProjectPickerModal.svelte` is issue #3365's project-selection
// surface — this covers what only mounting the real component can: the
// roster fetch driven by `open`, row-click -> `onSelect` wiring, the
// projectless affordance, and backdrop/close dismissal. Its pure logic
// (`fetchProjectRoster`/`isProjectRoster`/`projectCandidateLabel`) is
// covered by `lib/project-roster.test.ts`.
// What: Mounts the real component with `fetch` stubbed to answer
// `GET /projects`, mirroring `WorkstreamSwitcher.test.ts`'s stub-fetch
// mounting pattern.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import ProjectPickerModal from './ProjectPickerModal.svelte';
import type { ProjectSelection } from '../lib/new-workstream';

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

const ROSTER = {
  entries: [
    { name: 'trusty-tools', path: '/home/bob/trusty-mpm-projects/bobmatnyc/trusty-tools', owner: 'bobmatnyc' },
    { name: 'acme-api', path: '/home/bob/acme-api', owner: null },
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

function mountModal(props: {
  open: boolean;
  onSelect: (p: ProjectSelection | null) => void;
  onClose: () => void;
}) {
  instance = mount(ProjectPickerModal, { target, props }) as unknown as Record<string, unknown>;
}

describe('ProjectPickerModal', () => {
  it('renders nothing when closed', () => {
    mountModal({ open: false, onSelect: vi.fn(), onClose: vi.fn() });
    expect(target.querySelector('[role="dialog"]')).toBeNull();
  });

  it('fetches and renders the roster when opened', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/projects')) {
          return { ok: true, status: 200, json: async () => ROSTER } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );

    mountModal({ open: true, onSelect: vi.fn(), onClose: vi.fn() });

    await waitFor(() => target.textContent?.includes('bobmatnyc/trusty-tools') ?? false);
    expect(target.textContent).toContain('bobmatnyc/trusty-tools');
    expect(target.textContent).toContain('acme-api');
  });

  it('clicking a roster row calls onSelect with a resolved ProjectSelection', async () => {
    const onSelect = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 200, json: async () => ROSTER }) as Response),
    );

    mountModal({ open: true, onSelect, onClose: vi.fn() });
    await waitFor(() => target.textContent?.includes('acme-api') ?? false);

    const row = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'acme-api',
    ) as HTMLButtonElement;
    row.click();

    expect(onSelect).toHaveBeenCalledWith({
      path: '/home/bob/acme-api',
      displayPath: 'acme-api',
      isGitRepo: true,
    });
  });

  it('"start chatting without a project" calls onSelect(null)', async () => {
    const onSelect = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ entries: [] }) }) as Response),
    );

    mountModal({ open: true, onSelect, onClose: vi.fn() });
    await waitFor(() => target.textContent?.includes('start chatting without a project') ?? false);

    const button = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'start chatting without a project',
    ) as HTMLButtonElement;
    button.click();

    expect(onSelect).toHaveBeenCalledWith(null);
  });

  it('shows an empty-roster message distinct from the loading/error states', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ entries: [] }) }) as Response),
    );

    mountModal({ open: true, onSelect: vi.fn(), onClose: vi.fn() });
    await waitFor(() => target.textContent?.includes('no known projects found') ?? false);
    expect(target.textContent).toContain('no known projects found');
  });

  it('shows daemon-unreachable when GET /projects fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('Failed to fetch');
      }),
    );

    mountModal({ open: true, onSelect: vi.fn(), onClose: vi.fn() });
    await waitFor(() => target.textContent?.includes('daemon unreachable') ?? false);
    expect(target.textContent).toContain('daemon unreachable');
  });

  it('the backdrop and the × button both call onClose', async () => {
    const onClose = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 200, json: async () => ({ entries: [] }) }) as Response),
    );

    mountModal({ open: true, onSelect: vi.fn(), onClose });
    await waitFor(() => target.querySelector('[role="dialog"]') !== null);

    const closeButton = target.querySelector('[aria-label="close"]') as HTMLButtonElement;
    closeButton.click();
    expect(onClose).toHaveBeenCalledTimes(1);

    const backdrop = target.querySelector('[aria-label="close project picker"]') as HTMLButtonElement;
    backdrop.click();
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});
