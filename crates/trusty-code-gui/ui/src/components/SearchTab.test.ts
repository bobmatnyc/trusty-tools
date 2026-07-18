// Why: DOC-39 §4.7 AC-7.1 requires the Search tab to have NO input field —
// it is an audit trail, not a search box. This pins that invariant plus the
// component's four phases (connecting/daemon-unreachable/no-session/active),
// mirroring the phase-coverage style already established for
// `SessionMonitor.test.ts`.
// What: Mounts the real `SearchTab.svelte` with `fetch` stubbed for each
// phase and asserts the rendered text/DOM matches, plus that no `<input>` or
// `<textarea>` ever renders regardless of phase.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import SearchTab from './SearchTab.svelte';

const SESSION_ID = 'sess-search-tab';
const CREATED_AT = new Date().toISOString();

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

function noInputRendered(): boolean {
  return target.querySelector('input') === null && target.querySelector('textarea') === null;
}

/** jsdom's `textContent` preserves source whitespace/newlines verbatim (no
 * CSS-driven collapse) — normalize before substring assertions so template
 * line-wrapping doesn't break a test that only cares about the words. */
function normalizedText(): string {
  return (target.textContent ?? '').replace(/\s+/g, ' ').trim();
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

describe('SearchTab (DOC-39 §4.7, 10d)', () => {
  it('AC-7.1: renders no input field while daemon-unreachable', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: false, status: 503, json: async () => ({}) }),
    );
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('daemon unreachable') ?? false);

    expect(target.textContent).toContain('daemon unreachable');
    expect(noInputRendered()).toBe(true);
  });

  it('renders no-session state when the daemon has zero sessions', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.endsWith('/sessions')) {
          return { ok: true, status: 200, json: async () => ({ sessions: [] }) } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('nothing to audit yet') ?? false);

    expect(target.textContent).toContain('no active session');
    expect(noInputRendered()).toBe(true);
  });

  it('AC-7.2: renders the column headers and the honest gap notice (issue #3072) for an active session', async () => {
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
                { id: SESSION_ID, status: 'running', created_at: CREATED_AT, project: null },
              ],
            }),
          } as Response;
        }
        throw new Error(`unexpected fetch: ${url}`);
      }),
    );
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('issue #3072') ?? false);

    for (const column of ['lane', 'query', 'hits', 'latency', 'agent', 'age']) {
      expect(target.textContent).toContain(column);
    }
    expect(target.textContent).toContain('not yet implemented');
    expect(target.textContent).toContain('issue #3072');
    // AC-7.1 must hold in every phase, including the active/gap-notice one.
    expect(noInputRendered()).toBe(true);
  });

  it("AC-7.1: the explanatory banner is present and no input field ever renders", async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: false, status: 503, json: async () => ({}) }),
    );
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => normalizedText().includes("isn't a box you type in"));

    expect(normalizedText()).toContain("isn't a box you type in");
    expect(normalizedText()).toContain('audit trail of the searches your agents performed');
    expect(noInputRendered()).toBe(true);
  });
});
