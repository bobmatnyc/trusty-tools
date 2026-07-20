// Why: issue #3449 — pins `AgentsTab.svelte`'s connection phases, embedded
// vs disk-tier badge/remove-button behavior, the two-step inline remove
// confirm (mirroring `WorkstreamSwitcher.svelte`'s pattern), and the add
// flow's success/error paths.
// What: Mounts the real component with `fetch` stubbed per phase.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentsTab from './AgentsTab.svelte';

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

function normalizedText(): string {
  return (target.textContent ?? '').replace(/\s+/g, ' ').trim();
}

function agentsResponse(agents: unknown[]): Response {
  return { ok: true, status: 200, json: async () => ({ agents }) } as Response;
}

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
});

describe('AgentsTab', () => {
  it('renders the connecting state before the first poll resolves', async () => {
    vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(() => {})));
    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('connecting') ?? false);
    expect(target.textContent).toContain('connecting');
  });

  it('renders daemon-unreachable on a fetch failure', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 503 }));
    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('daemon unreachable') ?? false);
    expect(target.textContent).toContain('daemon unreachable');
  });

  it('renders embedded agents read-only, with no remove button', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        agentsResponse([
          { name: 'engineer', tier: 'embedded', description: 'Generalist agent', model: null },
        ]),
      ),
    );
    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => normalizedText().includes('engineer'));
    expect(normalizedText()).toContain('embedded');
    expect(target.querySelector('[aria-label="remove engineer"]')).toBeNull();
  });

  it('renders a broken disk entry with the broken badge and keeps the remove affordance (repair path)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        agentsResponse([
          {
            name: 'engineer',
            tier: 'broken',
            description: 'unparseable agent file — dispatch of this name will fail',
            model: null,
          },
        ]),
      ),
    );
    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => normalizedText().includes('engineer'));
    expect(normalizedText()).toContain('broken');
    expect(normalizedText()).not.toContain('embedded');
    // Deleting the bad file is the repair path — the remove button must stay.
    expect(target.querySelector('[aria-label="remove engineer"]')).not.toBeNull();
  });

  it('renders a remove button for a disk-tier agent and arms two-step confirm', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        agentsResponse([
          { name: 'my-agent', tier: 'project', description: null, model: null },
        ]),
      ),
    );
    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => normalizedText().includes('my-agent'));
    const removeButton = target.querySelector('[aria-label="remove my-agent"]') as HTMLButtonElement;
    expect(removeButton).not.toBeNull();

    removeButton.click();
    await waitFor(() => normalizedText().includes('confirm'));
    expect(normalizedText()).toContain('never mind');
  });

  it('deletes on confirm and refreshes the list', async () => {
    let deleted = false;
    const fetchMock = vi.fn((url: string, init?: RequestInit) => {
      if (url.endsWith('/agents') && (!init || init.method === undefined)) {
        return Promise.resolve(
          agentsResponse(
            deleted ? [] : [{ name: 'my-agent', tier: 'project', description: null, model: null }],
          ),
        );
      }
      if (init?.method === 'DELETE') {
        deleted = true;
        return Promise.resolve({ ok: true, status: 200 } as Response);
      }
      return Promise.resolve(agentsResponse([]));
    });
    vi.stubGlobal('fetch', fetchMock);

    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => normalizedText().includes('my-agent'));

    (target.querySelector('[aria-label="remove my-agent"]') as HTMLButtonElement).click();
    await waitFor(() => normalizedText().includes('confirm'));
    (Array.from(target.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'confirm') as HTMLButtonElement).click();

    await waitFor(() => normalizedText().includes('no agents found'));
    expect(normalizedText()).toContain('no agents found');
  });

  it('opens the add form and surfaces the daemon error message on a 403 collision', async () => {
    const fetchMock = vi.fn((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        return Promise.resolve({
          ok: false,
          status: 403,
          json: async () => ({ error: { message: "'engineer' is an embedded agent" } }),
        } as Response);
      }
      return Promise.resolve(agentsResponse([]));
    });
    vi.stubGlobal('fetch', fetchMock);

    instance = mount(AgentsTab, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => normalizedText().includes('no agents found'));

    (Array.from(target.querySelectorAll('button')).find((b) => b.textContent?.includes('add agent')) as HTMLButtonElement).click();
    await waitFor(() => target.querySelector('#agent-add-name') !== null);
    const nameInput = target.querySelector('#agent-add-name') as HTMLInputElement;
    const contentInput = target.querySelector('#agent-add-content') as HTMLTextAreaElement;
    nameInput.value = 'engineer';
    nameInput.dispatchEvent(new Event('input', { bubbles: true }));
    contentInput.value = '---\nname: engineer\n---\n\nBody.\n';
    contentInput.dispatchEvent(new Event('input', { bubbles: true }));

    function createButton(): HTMLButtonElement | undefined {
      return Array.from(target.querySelectorAll('button')).find(
        (b) => b.textContent?.trim() === 'create',
      ) as HTMLButtonElement | undefined;
    }
    await waitFor(() => createButton() !== undefined && !createButton()!.disabled);
    createButton()!.click();

    await waitFor(() => normalizedText().includes('embedded agent'));
    expect(normalizedText()).toContain('embedded agent');
  });
});
