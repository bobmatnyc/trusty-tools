// Why: issue #3449 — pins `SkillsTab.svelte`'s connection phases, bundled
// vs project-tier badge/remove-button behavior, the two-step inline remove
// confirm, the projectless add-flow lock (no user-level skill tier exists —
// see `crate::skills::protocol`'s docs), and the add flow's error path.
// What: Mounts the real component with `fetch` stubbed per phase.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import SkillsTab from './SkillsTab.svelte';

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

function skillsResponse(skills: unknown[]): Response {
  return { ok: true, status: 200, json: async () => ({ skills }) } as Response;
}

function sessionsResponse(hasProject: boolean): Response {
  return {
    ok: true,
    status: 200,
    json: async () => ({
      sessions: hasProject ? [{ id: 's-1', status: 'running', project: '/repo' }] : [],
    }),
  } as Response;
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

describe('SkillsTab', () => {
  it('renders the connecting state before the first poll resolves', async () => {
    vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(() => {})));
    instance = mount(SkillsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('connecting') ?? false);
    expect(target.textContent).toContain('connecting');
  });

  it('renders daemon-unreachable on a fetch failure', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 503 }));
    instance = mount(SkillsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('daemon unreachable') ?? false);
    expect(target.textContent).toContain('daemon unreachable');
  });

  it('renders bundled skills read-only, with no remove button, and locks the add flow projectless', async () => {
    const fetchMock = vi.fn((url: string) => {
      if (url.endsWith('/skills')) {
        return Promise.resolve(
          skillsResponse([
            { name: 'systematic-debugging', tier: 'bundled', description: 'Debug workflow' },
          ]),
        );
      }
      return Promise.resolve(sessionsResponse(false));
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SkillsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => normalizedText().includes('systematic-debugging'));
    expect(normalizedText()).toContain('bundled');
    expect(target.querySelector('[aria-label="remove systematic-debugging"]')).toBeNull();

    const addButton = Array.from(target.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('add skill'),
    ) as HTMLButtonElement;
    await waitFor(() => addButton.disabled === true);
    expect(addButton.disabled).toBe(true);
    expect(normalizedText()).toContain('project-scoped');
  });

  it('renders a remove button for a project-tier skill and arms two-step confirm', async () => {
    const fetchMock = vi.fn((url: string) => {
      if (url.endsWith('/skills')) {
        return Promise.resolve(
          skillsResponse([{ name: 'my-skill', tier: 'project', description: '' }]),
        );
      }
      return Promise.resolve(sessionsResponse(true));
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SkillsTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => normalizedText().includes('my-skill'));
    const removeButton = target.querySelector('[aria-label="remove my-skill"]') as HTMLButtonElement;
    expect(removeButton).not.toBeNull();

    removeButton.click();
    await waitFor(() => normalizedText().includes('confirm'));
    expect(normalizedText()).toContain('never mind');
  });

  it('deletes on confirm and refreshes the list', async () => {
    let deleted = false;
    const fetchMock = vi.fn((url: string, init?: RequestInit) => {
      if (url.endsWith('/skills') && (!init || init.method === undefined)) {
        return Promise.resolve(
          skillsResponse(
            deleted ? [] : [{ name: 'my-skill', tier: 'project', description: '' }],
          ),
        );
      }
      if (init?.method === 'DELETE') {
        deleted = true;
        return Promise.resolve({ ok: true, status: 200 } as Response);
      }
      return Promise.resolve(sessionsResponse(true));
    });
    vi.stubGlobal('fetch', fetchMock);

    instance = mount(SkillsTab, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => normalizedText().includes('my-skill'));

    (target.querySelector('[aria-label="remove my-skill"]') as HTMLButtonElement).click();
    await waitFor(() => normalizedText().includes('confirm'));
    (Array.from(target.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'confirm') as HTMLButtonElement).click();

    await waitFor(() => normalizedText().includes('no skills found'));
    expect(normalizedText()).toContain('no skills found');
  });

  it('opens the add form (project bound) and surfaces the daemon error message on a 403 collision', async () => {
    const fetchMock = vi.fn((url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        return Promise.resolve({
          ok: false,
          status: 403,
          json: async () => ({ error: { message: "'systematic-debugging' is a bundled skill" } }),
        } as Response);
      }
      if (url.endsWith('/skills')) return Promise.resolve(skillsResponse([]));
      return Promise.resolve(sessionsResponse(true));
    });
    vi.stubGlobal('fetch', fetchMock);

    instance = mount(SkillsTab, { target }) as unknown as Record<string, unknown>;
    await waitFor(() => normalizedText().includes('no skills found'));

    const addButton = Array.from(target.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('add skill'),
    ) as HTMLButtonElement;
    await waitFor(() => addButton.disabled === false);
    addButton.click();

    await waitFor(() => target.querySelector('#skill-add-name') !== null);
    const nameInput = target.querySelector('#skill-add-name') as HTMLInputElement;
    const contentInput = target.querySelector('#skill-add-content') as HTMLTextAreaElement;
    nameInput.value = 'systematic-debugging';
    nameInput.dispatchEvent(new Event('input', { bubbles: true }));
    contentInput.value = '---\nname: systematic-debugging\n---\n\nBody.\n';
    contentInput.dispatchEvent(new Event('input', { bubbles: true }));

    function createButton(): HTMLButtonElement | undefined {
      return Array.from(target.querySelectorAll('button')).find(
        (b) => b.textContent?.trim() === 'create',
      ) as HTMLButtonElement | undefined;
    }
    await waitFor(() => createButton() !== undefined && !createButton()!.disabled);
    createButton()!.click();

    await waitFor(() => normalizedText().includes('bundled skill'));
    expect(normalizedText()).toContain('bundled skill');
  });
});
