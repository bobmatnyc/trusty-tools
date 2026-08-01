// Why (#4281): the acceptance criterion is a LAUNCH-boundary property —
// "relaunching the app restores the previously selected assistant" — and the
// seed happens at MODULE-IMPORT time, which no in-module `.set()` can
// simulate. Each test here therefore writes localStorage, resets the module
// registry, and re-imports `stores/app.ts`: that re-import IS the app launch.
// The codec's own degradation contract is covered by
// `lib/selectedAssistant.test.ts`; this file pins the wiring — seed,
// write-through, and stale-pointer reconciliation against the live roster.
// What: store-level tests over `activeAgentId` against a stubbed
// `globalThis.localStorage`. The stub is not a convenience — under Node 26 the
// runtime defines its own `localStorage` global (undefined unless
// `--localstorage-file` is passed), which shadows jsdom's, so `localStorage`
// is genuinely absent in this environment. That is itself worth knowing: the
// production code's "no storage ⇒ no persistence, never a throw" guard is
// exercised by every OTHER test file in this suite for free.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { SELECTED_ASSISTANT_KEY } from '../lib/selectedAssistant';

/** Minimal in-memory `Storage` stand-in for `globalThis.localStorage`. */
function installFakeLocalStorage(): Map<string, string> {
  const map = new Map<string, string>();
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, String(v));
    },
    removeItem: (k: string) => {
      map.delete(k);
    },
    clear: () => map.clear(),
  });
  return map;
}

let store: Map<string, string>;

/** Re-imports `stores/app.ts` with a fresh module registry — one app launch. */
async function relaunch() {
  vi.resetModules();
  return await import('./app');
}

beforeEach(() => {
  store = installFakeLocalStorage();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe('activeAgentId persistence across launches (#4281)', () => {
  it('defaults to Concierge on a first launch with no persisted selection', async () => {
    const app = await relaunch();
    expect(get(app.activeAgentId)).toBeNull();
  });

  it('restores the previously selected assistant instance on relaunch', async () => {
    const first = await relaunch();
    first.activeAgentId.set('cto-assistant');
    expect(store.get(SELECTED_ASSISTANT_KEY) ?? null).toBe('cto-assistant');

    const second = await relaunch();
    expect(get(second.activeAgentId)).toBe('cto-assistant');
  });

  it('restores an explicit Concierge selection on relaunch', async () => {
    const first = await relaunch();
    first.activeAgentId.set('izzie');
    first.activeAgentId.set(null);
    expect(store.get(SELECTED_ASSISTANT_KEY) ?? null).toBe('ctrl');

    const second = await relaunch();
    expect(get(second.activeAgentId)).toBeNull();
  });

  it('falls back to Concierge when the persisted selection is corrupt', async () => {
    store.set(SELECTED_ASSISTANT_KEY, '{"agent":"izzie"}');
    const app = await relaunch();
    expect(get(app.activeAgentId)).toBeNull();
  });

  it('demotes a stale selection to Concierge once a populated roster contradicts it', async () => {
    store.set(SELECTED_ASSISTANT_KEY, 'deleted-assistant');
    const app = await relaunch();
    // Survives until the roster actually arrives — nothing has contradicted it yet.
    expect(get(app.activeAgentId)).toBe('deleted-assistant');

    app.catalogAgents.set([{ name: 'izzie' }]);
    expect(get(app.activeAgentId)).toBeNull();
    expect(store.get(SELECTED_ASSISTANT_KEY) ?? null).toBe('ctrl');
  });

  it('keeps a selection the roster confirms', async () => {
    store.set(SELECTED_ASSISTANT_KEY, 'izzie');
    const app = await relaunch();
    app.catalogAgents.set([{ name: 'izzie' }, { name: 'cto-assistant' }]);
    expect(get(app.activeAgentId)).toBe('izzie');
  });

  it('never discards a selection when the catalog fetch yields nothing', async () => {
    // The cold-start race (App.svelte's `refetchPickerCatalogsOnReady`): the
    // first `/api/agents` call can fail outright, leaving the roster empty.
    // That must not read as "your assistant is gone".
    store.set(SELECTED_ASSISTANT_KEY, 'izzie');
    const app = await relaunch();
    app.catalogAgents.set([]);
    app.overlayAgents.set([]);
    expect(get(app.activeAgentId)).toBe('izzie');
  });

  it('persists a selection made through an overlay entry', async () => {
    const app = await relaunch();
    app.overlayAgents.set([{ slug: 'my-cto', name: 'My CTO' }]);
    app.activeAgentId.set('my-cto');
    expect(store.get(SELECTED_ASSISTANT_KEY) ?? null).toBe('my-cto');
    expect(get(app.activeAgentId)).toBe('my-cto');
  });

  it('does not re-scope the selection when a workstream filter changes', async () => {
    // Owner decision 2026-07-28 / DOC-54 §9.2: workstreams are FILTERS, not
    // containers — filtering must never re-select a different assistant.
    // `activeProjectId` is the only workstream-ish axis in this store and is
    // deliberately independent of `activeAgentId`.
    store.set(SELECTED_ASSISTANT_KEY, 'izzie');
    const app = await relaunch();
    app.catalogAgents.set([{ name: 'izzie' }]);
    app.activeProjectId.set('some-other-project');
    expect(get(app.activeAgentId)).toBe('izzie');
    expect(store.get(SELECTED_ASSISTANT_KEY) ?? null).toBe('izzie');
  });
});
