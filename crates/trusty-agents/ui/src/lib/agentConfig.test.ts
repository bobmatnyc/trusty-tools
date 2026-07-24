// Unit tests for the pure parts of `agentConfig.ts` (#3819, epic #3052).
// The fetch/patch wrappers are thin REST glue with no branching logic worth
// unit-testing in isolation (mirrors `stores/app.ts`'s `fetchAgentCatalog`/
// `fetchModelCatalog`, which are likewise doc-commented "Test: manual" — see
// `agentConfig.ts`'s own doc comments); these tests cover the scaffolding
// shapes the concept-demo config pane renders.

import { describe, expect, it } from 'vitest';
import { DEFINED_LISTENERS, scaffoldOkgStores } from './agentConfig';

describe('DEFINED_LISTENERS', () => {
  it('defines exactly the gmail and google-calendar listeners', () => {
    const ids = DEFINED_LISTENERS.map((l) => l.id);
    expect(ids).toEqual(['gmail', 'google-calendar']);
  });

  it('every listener declares at least one event type', () => {
    for (const listener of DEFINED_LISTENERS) {
      expect(listener.eventTypes.length).toBeGreaterThan(0);
    }
  });
});

describe('scaffoldOkgStores', () => {
  it('names the scaffold store after the agent', () => {
    const stores = scaffoldOkgStores('izzie');
    expect(stores).toHaveLength(1);
    expect(stores[0].name).toBe('izzie-kb');
  });

  it('marks the scaffold store as not connected (honest, not fabricated)', () => {
    const stores = scaffoldOkgStores('cto-assistant');
    expect(stores[0].connected).toBe(false);
  });
});
