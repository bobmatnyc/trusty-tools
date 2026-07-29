// Regression test for a #4290 code-review finding (HIGH): only bootstrap()'s
// initial `/kg/subjects` call checked `env.connected` before assigning data.
// `loadAll`, `loadCount`, and `loadSubject` assigned `env.data` unconditionally,
// so a mid-session `connected: false` response (daemon restart, palace
// unbound — still HTTP 200 with `data: []`/`{active: 0}`) rendered as an
// empty/zero result indistinguishable from a genuinely-connected-but-empty
// palace.
//
// Why: This is the most important test in the KG browser's coverage — it's
// the one the owner's contract exists specifically to prevent (a user must
// never see "no data" when the real cause is a stopped daemon). Mounts the
// real component (mirrors `ChatPane.test.ts`'s stub-fetch mounting pattern)
// so the assertion is against actual rendered DOM, not the internal state.
// What: Two scenarios — (1) `loadAll`/`loadCount` (the `Promise.all` pair
// bootstrap kicks off once `/kg/subjects` reports connected) both degrade,
// (2) a user click on a subject (`loadSubject`) degrades after bootstrap
// already rendered live data. Both must produce the SAME disconnected copy
// bootstrap's own `connected: false` path renders, carrying `reason`.
// Test: this file.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import KnowledgeGraphBrowser from './KnowledgeGraphBrowser.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

interface Routes {
  subjects: unknown;
  all: unknown;
  count: unknown;
  subject?: unknown;
}

function jsonResponse(body: unknown) {
  return { ok: true, status: 200, json: async () => body } as Response;
}

/** Routes each of the four KG proxy endpoints to its own canned envelope. */
function stubRoutes(routes: Routes) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/kg/subjects')) return jsonResponse(routes.subjects);
      if (url.includes('/kg/all')) return jsonResponse(routes.all);
      if (url.includes('/kg/count')) return jsonResponse(routes.count);
      if (url.includes('/kg?')) return jsonResponse(routes.subject);
      throw new Error(`unexpected fetch in KnowledgeGraphBrowser test: ${url}`);
    }),
  );
}

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('timed out waiting for condition');
}

function render(agentName = 'izzie') {
  instance = mount(KnowledgeGraphBrowser, {
    target,
    props: { agentName, onClose: () => {} },
  }) as unknown as Record<string, unknown>;
}

const panelText = () => target.textContent ?? '';
const subjectButtons = () => Array.from(target.querySelectorAll('aside li button'));

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

describe('KnowledgeGraphBrowser — mid-session disconnection (#4290)', () => {
  it('loadAll/loadCount degrading after a connected bootstrap render the disconnected reason, never empty data', async () => {
    stubRoutes({
      subjects: {
        palace: 'owner-profile',
        connected: true,
        data: [{ subject: 'bob', count: 2 }],
      },
      // The subjects call resolved connected:true, but by the time the
      // Promise.all([loadAll, loadCount]) pair it kicks off lands, the
      // daemon has gone away — exactly the ticket's "daemon restarts
      // mid-session" scenario.
      all: {
        palace: 'owner-profile',
        connected: false,
        reason: 'trusty-memory daemon is unreachable',
        data: [],
      },
      count: {
        palace: 'owner-profile',
        connected: false,
        reason: 'trusty-memory daemon is unreachable',
        data: { active: 0 },
      },
    });

    render();
    await waitFor(() => panelText().includes('trusty-memory daemon is unreachable'));

    expect(panelText()).not.toContain('No triples.');
    expect(panelText()).not.toContain('active triples');
  });

  it('loadSubject degrading after a connected bootstrap renders the disconnected reason, not "No triples."', async () => {
    stubRoutes({
      subjects: {
        palace: 'owner-profile',
        connected: true,
        data: [{ subject: 'bob', count: 2 }],
      },
      all: {
        palace: 'owner-profile',
        connected: true,
        data: [{ subject: 'bob', predicate: 'likes', object: 'cats' }],
      },
      count: { palace: 'owner-profile', connected: true, data: { active: 5 } },
      // The daemon goes away between the initial connected bootstrap and the
      // user clicking a subject row.
      subject: {
        palace: 'owner-profile',
        connected: false,
        reason: 'palace unbound mid-session',
        data: [],
      },
    });

    render();
    await waitFor(() => subjectButtons().length > 0);
    expect(panelText()).toContain('5 active triples');

    (subjectButtons()[0] as HTMLButtonElement).click();

    await waitFor(() => panelText().includes('palace unbound mid-session'));
    expect(panelText()).not.toContain('No triples.');
  });

  it('a genuinely connected, empty palace renders the empty copy, not the disconnected one', async () => {
    stubRoutes({
      subjects: { palace: 'owner-profile', connected: true, data: [] },
      all: { palace: 'owner-profile', connected: true, data: [] },
      count: { palace: 'owner-profile', connected: true, data: { active: 0 } },
    });

    render();
    const normalized = () => panelText().replace(/\s+/g, ' ');
    await waitFor(() => normalized().includes('Knowledge Graph has no triples yet'));

    expect(normalized()).not.toContain('is not reachable right now');
  });
});
