// Regression test for a #4290 code-review finding (HIGH): only bootstrap()'s
// initial `/kg/subjects` call checked `env.connected` before assigning data.
// `loadAll`, `loadCount`, and `loadSubject` assigned `env.data` unconditionally,
// so a mid-session `connected: false` response (the OKG tree moved or the
// binding rewritten — still HTTP 200 with `data: []`/`{active: 0}`) rendered
// as an empty/zero result indistinguishable from a genuinely-connected-but-
// empty tree.
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
        tree: '/homes/izzie/okg',
        connected: true,
        data: [{ subject: 'bob', count: 2 }],
      },
      // The subjects call resolved connected:true, but by the time the
      // Promise.all([loadAll, loadCount]) pair it kicks off lands, the
      // daemon has gone away — exactly the ticket's "daemon restarts
      // mid-session" scenario.
      all: {
        tree: '/homes/izzie/okg',
        connected: false,
        reason: 'the OKG tree is unreadable',
        data: [],
      },
      count: {
        tree: '/homes/izzie/okg',
        connected: false,
        reason: 'the OKG tree is unreadable',
        data: { active: 0, definition_count: 0 },
      },
    });

    render();
    await waitFor(() => panelText().includes('the OKG tree is unreadable'));

    expect(panelText()).not.toContain('No triples.');
    expect(panelText()).not.toContain('active triples');
  });

  it('loadSubject degrading after a connected bootstrap renders the disconnected reason, not "No triples."', async () => {
    stubRoutes({
      subjects: {
        tree: '/homes/izzie/okg',
        connected: true,
        data: [{ subject: 'bob', count: 2 }],
      },
      all: {
        tree: '/homes/izzie/okg',
        connected: true,
        data: [{ subject: 'bob', predicate: 'likes', object: 'cats' }],
      },
      count: { tree: '/homes/izzie/okg', connected: true, data: { active: 5, definition_count: 1 } },
      // The daemon goes away between the initial connected bootstrap and the
      // user clicking a subject row.
      subject: {
        tree: '/homes/izzie/okg',
        connected: false,
        reason: 'the OKG tree went away mid-session',
        data: [],
      },
    });

    render();
    await waitFor(() => subjectButtons().length > 0);
    expect(panelText()).toContain('5 active triples');

    (subjectButtons()[0] as HTMLButtonElement).click();

    await waitFor(() => panelText().includes('the OKG tree went away mid-session'));
    expect(panelText()).not.toContain('No triples.');
  });

  it('a genuinely connected, empty tree renders the empty copy, not the disconnected one', async () => {
    stubRoutes({
      subjects: { tree: '/homes/izzie/okg', connected: true, data: [] },
      all: { tree: '/homes/izzie/okg', connected: true, data: [] },
      count: { tree: '/homes/izzie/okg', connected: true, data: { active: 0, definition_count: 0 } },
    });

    render();
    const normalized = () => panelText().replace(/\s+/g, ' ');
    await waitFor(() => normalized().includes('is readable, but holds nothing yet'));

    expect(normalized()).not.toContain('is not reachable right now');
  });
});

it('keeps the selected subject when an older all-triples request finishes late', async () => {
  let finishAll!: (response: Response) => void;
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/kg/subjects')) return jsonResponse({ connected: true, tree: 'p', data: [{ subject: 'selected', count: 1 }] });
    if (url.includes('/kg/all')) return new Promise<Response>(resolve => finishAll = resolve);
    if (url.includes('/kg/count')) return jsonResponse({ connected: true, tree: 'p', data: { active: 1, definition_count: 1 } });
    return jsonResponse({ connected: true, tree: 'p', data: [{ subject: 'selected', predicate: 'has', object: 'current value' }] });
  }));
  render();
  await waitFor(() => subjectButtons().length === 1 && !!finishAll);
  (subjectButtons()[0] as HTMLButtonElement).click();
  await waitFor(() => panelText().includes('current value'));
  finishAll(jsonResponse({ connected: true, tree: 'p', data: [{ subject: 'old', predicate: 'has', object: 'stale value' }] }));
  await new Promise(resolve => setTimeout(resolve, 20));
  expect(panelText()).toContain('current value');
  expect(panelText()).not.toContain('stale value');
});


// #7430: the exposed graph carries BOTH halves — the triples and the
// definitions that say what each subject IS. A browser that rendered only the
// triple table would satisfy the word "graph" and fail the owner's closure
// condition, so the definition list is asserted in the DOM.
it('renders the definitions that come with a page of triples', async () => {
  stubRoutes({
    subjects: { tree: '/homes/izzie/okg', connected: true, data: [{ subject: 'Bob', count: 1 }] },
    all: {
      tree: '/homes/izzie/okg',
      source: 'okg',
      connected: true,
      data: [{ subject: 'Bob', predicate: 'works_at', object: 'Duetto', provenance: 'people/bob.md' }],
      definitions: [
        { subject: 'Bob', collection: 'people', slug: 'bob', type: 'Person', summary: 'The owner.', path: 'people/bob.md' },
      ],
    },
    count: { tree: '/homes/izzie/okg', connected: true, data: { active: 1, definition_count: 1 } },
  });

  render();
  await waitFor(() => target.querySelector('[data-kg-definitions]') !== null);
  const defs = target.querySelector('[data-kg-definitions]')?.textContent ?? '';
  expect(defs).toContain('Person');
  expect(defs).toContain('The owner.');
  // And the triple's source file, which replaced the memory-only confidence
  // and valid-from columns.
  expect(panelText()).toContain('people/bob.md');
});

it('provides an X close control for returning to chat', async () => {
  stubRoutes({ subjects: { connected: false, tree: null, data: [] }, all: {}, count: {} });
  const onClose = vi.fn();
  instance = mount(KnowledgeGraphBrowser, { target, props: { agentName: 'izzie', onClose } }) as unknown as Record<string, unknown>;
  await waitFor(() => target.querySelector('[aria-label="Close Knowledge Graph"]') !== null);
  (target.querySelector('[aria-label="Close Knowledge Graph"]') as HTMLButtonElement).click();
  expect(onClose).toHaveBeenCalledOnce();
});
