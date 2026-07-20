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

    expect(target.textContent).toContain('no active workstream');
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

  it('issue #3111: renders the valid subset plus an omitted-rows indicator for a partially-valid payload', async () => {
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
              // Malformed: lane must be a string. Must not take the whole
              // response down with it (issue #3111 MEDIUM).
              {
                kind: 'search',
                agent: 'engineer',
                agent_id: 'ag-4',
                lane: 42,
                query: 'bad row',
                hit_count: 1,
                latency_ms: 10,
                at: new Date().toISOString(),
              },
              // Unrecognized future kind — tolerated as omitted, not fatal.
              {
                kind: 'graph_traverse',
                agent: 'engineer',
                agent_id: 'ag-5',
                query: 'call graph',
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

    await waitFor(() => normalizedText().includes('rows omitted'));

    // The one valid row still renders.
    expect(target.textContent).toContain('parse config');
    // Two rows dropped (the bad `lane` and the unrecognized `kind`). jsdom's
    // `textContent` preserves the template's source whitespace/line-wrapping
    // verbatim (see `normalizedText()`'s own doc comment), so this asserts
    // against the whitespace-collapsed text, not a raw substring.
    expect(normalizedText()).toContain('2 rows omitted');
    // Never the all-or-nothing "audit unavailable" for a partially-valid payload.
    expect(target.textContent).not.toContain('audit unavailable');
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

    expect(target.textContent).toContain('no active workstream');
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

  it('issue #3111 LOW: recovers on the next poll once the daemon stops returning 500 for search-audit', async () => {
    vi.useFakeTimers();
    try {
      let auditCalls = 0;
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.endsWith('/sessions')) return sessionsResponse();
        if (url.endsWith(`/sessions/${SESSION_ID}/search-audit`)) {
          auditCalls += 1;
          if (auditCalls === 1) {
            return { ok: false, status: 500, json: async () => ({}) } as Response;
          }
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
                  query: 'recovered query',
                  hit_count: 3,
                  latency_ms: 15,
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

      // Flush the initial poll (poll #1: HTTP 500 -> "audit unavailable").
      await vi.advanceTimersByTimeAsync(0);
      expect(target.textContent).toContain('audit unavailable');
      expect(auditCalls).toBe(1);

      // Advance past POLL_MS (5000ms, matching StatusBar.svelte/WorkstreamActivity.svelte)
      // to trigger poll #2, which now succeeds.
      await vi.advanceTimersByTimeAsync(5000);
      expect(auditCalls).toBe(2);
      expect(target.textContent).toContain('recovered query');
      expect(target.textContent).not.toContain('audit unavailable');
    } finally {
      vi.useRealTimers();
    }
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
