// Why: DOC-39 §4.7 AC-7.1 requires the Search tab to have NO input field —
// it is an audit trail, not a search box. This pins that invariant plus the
// component's four connection phases (connecting/daemon-unreachable/
// no-session/active), and now that `GET /sessions/{id}/search-audit`
// (issue #3072, PR #3107) is wired in, the audit-row states: populated,
// empty, and malformed-body degradation (the runtime shape guard from
// `lib/search-audit.ts` must stop a schema-drifted response before it
// crashes the tab).
// What: Mounts the real `SearchTab.svelte` with `fetch` stubbed for each
// phase, asserting the rendered DOM/text and the exact URLs fetched, plus
// that no `<input>` or `<textarea>` ever renders regardless of phase.
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

/** A `GET /sessions` stub returning exactly one running session. */
function sessionsResponse(): Response {
  return {
    ok: true,
    status: 200,
    json: async () => ({
      sessions: [{ id: SESSION_ID, status: 'running', created_at: CREATED_AT, project: null }],
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

describe('SearchTab (DOC-39 §4.7, 10d)', () => {
  it('renders the connecting state before the first poll resolves', async () => {
    vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(() => {})));
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('connecting') ?? false);

    expect(target.textContent).toContain('connecting');
    expect(noInputRendered()).toBe(true);
  });

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
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) {
        return { ok: true, status: 200, json: async () => ({ sessions: [] }) } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('nothing to audit yet') ?? false);

    expect(target.textContent).toContain('no active session');
    expect(noInputRendered()).toBe(true);
    // No search-audit fetch should ever be attempted with no active session.
    expect(fetchMock.mock.calls.some(([u]) => String(u).includes('search-audit'))).toBe(false);
  });

  it('AC-7.2: fetches search-audit for the active session and renders real rows', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) return sessionsResponse();
      if (url.endsWith(`/sessions/${SESSION_ID}/search-audit`)) {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            search_audit: [
              {
                kind: 'search',
                agent: 'engineer',
                agent_id: 'ag-1',
                lane: 'semantic',
                query: 'parse config',
                hit_count: 7,
                latency_ms: 42,
                at: new Date().toISOString(),
              },
              {
                kind: 'recall',
                agent: 'pm',
                agent_id: 'ag-2',
                query: 'prior decisions',
                result_count: 5,
                injected_count: 3,
                at: new Date().toISOString(),
              },
            ],
          }),
        } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('parse config') ?? false);

    for (const column of ['lane', 'query', 'hits', 'latency', 'agent', 'age']) {
      expect(target.textContent).toContain(column);
    }
    // Search row: real lane, hit count, latency.
    expect(target.textContent).toContain('semantic');
    expect(target.textContent).toContain('parse config');
    expect(target.textContent).toContain('7');
    expect(target.textContent).toContain('42ms');
    expect(target.textContent).toContain('engineer');
    // Recall row: normalized 'recall' lane label, no latency, combined counts.
    expect(target.textContent).toContain('recall');
    expect(target.textContent).toContain('prior decisions');
    expect(target.textContent).toContain('5 (3 injected)');
    expect(target.textContent).toContain('pm');

    expect(
      fetchMock.mock.calls.some(([u]) => String(u).endsWith(`/sessions/${SESSION_ID}/search-audit`)),
    ).toBe(true);
    expect(noInputRendered()).toBe(true);
  });

  it('renders the empty-list state honestly when a session has no search activity yet', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) return sessionsResponse();
      if (url.endsWith(`/sessions/${SESSION_ID}/search-audit`)) {
        return { ok: true, status: 200, json: async () => ({ search_audit: [] }) } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('no searches recorded yet') ?? false);

    expect(target.textContent).toContain('no searches recorded yet');
    expect(noInputRendered()).toBe(true);
  });

  it('degrades to an error row instead of throwing when the audit body is malformed', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) return sessionsResponse();
      if (url.endsWith(`/sessions/${SESSION_ID}/search-audit`)) {
        // Missing `search_audit` entirely — schema drift / interposed proxy.
        return { ok: true, status: 200, json: async () => ({ unexpected: true }) } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('audit unavailable') ?? false);

    expect(target.textContent).toContain('audit unavailable');
    expect(target.textContent).toContain('malformed response');
    expect(noInputRendered()).toBe(true);
  });

  it('treats a 404 on search-audit as the session having vanished mid-poll', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) return sessionsResponse();
      if (url.endsWith(`/sessions/${SESSION_ID}/search-audit`)) {
        return { ok: false, status: 404, json: async () => ({}) } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('nothing to audit yet') ?? false);

    expect(target.textContent).toContain('no active session');
    expect(noInputRendered()).toBe(true);
  });

  it('treats a non-200/non-404 audit status as a recoverable partial error, not a crash', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/sessions')) return sessionsResponse();
      if (url.endsWith(`/sessions/${SESSION_ID}/search-audit`)) {
        return { ok: false, status: 500, json: async () => ({}) } as Response;
      }
      throw new Error(`unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    instance = mount(SearchTab, { target }) as unknown as Record<string, unknown>;

    await waitFor(() => target.textContent?.includes('audit unavailable') ?? false);

    expect(target.textContent).toContain('audit unavailable');
    expect(target.textContent).toContain('HTTP 500');
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
