// `fetchModelCatalog`'s body-shape contract (#7456).
//
// Why: `ModelSwitcher.svelte` already renders a "Default"-only picker when
// `modelCatalog` is `null`, and `fetchModelCatalog`'s doc comment promises
// that fallback covers an unreachable API. It did not cover a 200 whose body
// was not a catalog: the object was stored verbatim, and `buildPicker`'s
// `catalog.providers.filter(…)` then threw DURING RENDER. A throw there aborts
// the whole `ChatPane` mount, not just the switcher — which is how a bad
// `/api/models` response left the browser stranded on the assistant picker
// with the Chat tab already selected.
// What: a well-formed body populates the store; a body missing `providers` or
// `local` leaves it `null` and rejects, so the existing null branch runs.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import { fetchModelCatalog, modelCatalog } from './app';

const CATALOG = {
  providers: [
    {
      provider_id: 'openrouter',
      default_model: 'anthropic/claude-sonnet-4-6',
      context_window: 200000,
      credential_configured: true,
      reachable_today: true,
    },
  ],
  local: {
    provider_id: 'ollama',
    default_model: 'qwen2.5',
    available: false,
    reachable_today: false,
  },
};

function respond(body: unknown, ok = true) {
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok, status: ok ? 200 : 500, json: async () => body })));
}

beforeEach(() => {
  modelCatalog.set(null);
});

afterEach(() => {
  vi.unstubAllGlobals();
  modelCatalog.set(null);
});

describe('fetchModelCatalog (#7456)', () => {
  it('stores a well-formed catalog', async () => {
    respond(CATALOG);
    await fetchModelCatalog();
    expect(get(modelCatalog)?.providers).toHaveLength(1);
  });

  it('rejects a 200 body with no providers array and leaves the store null', async () => {
    respond({});
    await expect(fetchModelCatalog()).rejects.toThrow(/providers\/local catalog/);
    expect(get(modelCatalog)).toBeNull();
  });

  it('rejects a 200 body with providers but no local entry', async () => {
    respond({ providers: [] });
    await expect(fetchModelCatalog()).rejects.toThrow(/providers\/local catalog/);
    expect(get(modelCatalog)).toBeNull();
  });

  it('clears a previously good catalog rather than leaving a stale one', async () => {
    respond(CATALOG);
    await fetchModelCatalog();
    respond({});
    await expect(fetchModelCatalog()).rejects.toThrow();
    expect(get(modelCatalog)).toBeNull();
  });
});
