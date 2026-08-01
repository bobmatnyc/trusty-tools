// Why (#4281): the persisted selection is a file the user can edit, delete,
// or point at an assistant that no longer exists — and startup must survive
// all three without panicking or blocking. These tests pin the degradation
// contract for the codec itself; `stores/selectedAssistant.store.test.ts`
// pins the store wiring on top of it.
// What: unit tests over `lib/selectedAssistant.ts` with an injected
// `SelectionStorage` (no jsdom `localStorage` dependency), including a
// storage that throws on every operation.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  SELECTED_ASSISTANT_KEY,
  defaultSelectionStorage,
  persistSelectedAssistant,
  readSelectedAssistant,
  reconcileSelectedAssistant,
  type SelectionStorage,
} from './selectedAssistant';
import type { RosterEntry } from './roster';

/** In-memory `SelectionStorage`, optionally pre-seeded. */
function fakeStorage(seed?: string): SelectionStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  if (seed !== undefined) map.set(SELECTED_ASSISTANT_KEY, seed);
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => {
      map.set(k, v);
    },
    removeItem: (k) => {
      map.delete(k);
    },
  };
}

/** Storage that throws on every operation (Safari private mode, quota). */
const throwingStorage: SelectionStorage = {
  getItem() {
    throw new Error('SecurityError');
  },
  setItem() {
    throw new Error('QuotaExceededError');
  },
  removeItem() {
    throw new Error('SecurityError');
  },
};

function entry(id: string): RosterEntry {
  return { id, label: id, source: 'catalog', kind: 'assistant' };
}

describe('selected-assistant codec (#4281)', () => {
  it('readSelectedAssistant_absent_key_is_concierge', () => {
    expect(readSelectedAssistant(fakeStorage())).toBeNull();
  });

  it('readSelectedAssistant_returns_a_persisted_instance_id', () => {
    expect(readSelectedAssistant(fakeStorage('cto-assistant'))).toBe('cto-assistant');
  });

  it('readSelectedAssistant_ctrl_sentinel_is_concierge', () => {
    // Concierge is `activeAgentId === null`; dispatching it BY NAME would
    // route through the tools-OFF persona path, so the sentinel must decode
    // back to null rather than to the string 'ctrl'.
    expect(readSelectedAssistant(fakeStorage('ctrl'))).toBeNull();
  });

  it('readSelectedAssistant_corrupt_value_is_concierge_and_evicted', () => {
    for (const corrupt of ['', '   ', '{"id":"izzie"}', 'izzie izzie', '../../etc/passwd', 'x'.repeat(65)]) {
      const storage = fakeStorage(corrupt);
      expect(readSelectedAssistant(storage)).toBeNull();
      // Self-heals: the unreadable value is not left to be re-parsed forever.
      expect(storage.map.has(SELECTED_ASSISTANT_KEY)).toBe(false);
    }
  });

  it('readSelectedAssistant_survives_throwing_storage', () => {
    expect(() => readSelectedAssistant(throwingStorage)).not.toThrow();
    expect(readSelectedAssistant(throwingStorage)).toBeNull();
    expect(readSelectedAssistant(null)).toBeNull();
  });

  it('persistSelectedAssistant_round_trips_an_instance_id', () => {
    const storage = fakeStorage();
    persistSelectedAssistant('izzie', storage);
    expect(storage.map.get(SELECTED_ASSISTANT_KEY)).toBe('izzie');
    expect(readSelectedAssistant(storage)).toBe('izzie');
  });

  it('persistSelectedAssistant_encodes_concierge_as_the_ctrl_sentinel', () => {
    const storage = fakeStorage('izzie');
    persistSelectedAssistant(null, storage);
    expect(storage.map.get(SELECTED_ASSISTANT_KEY)).toBe('ctrl');
    expect(readSelectedAssistant(storage)).toBeNull();
  });

  it('persistSelectedAssistant_survives_throwing_storage', () => {
    expect(() => persistSelectedAssistant('izzie', throwingStorage)).not.toThrow();
    expect(() => persistSelectedAssistant(null, null)).not.toThrow();
  });

  it('defaultSelectionStorage_returns_null_when_unavailable', () => {
    // No stub is installed here, and this environment genuinely has no
    // `localStorage` (Node 26's own global shadows jsdom's) — so this is the
    // real absent-storage path, not a simulation of it.
    expect(defaultSelectionStorage()).toBeNull();
    expect(readSelectedAssistant()).toBeNull();
    expect(() => persistSelectedAssistant('izzie')).not.toThrow();
  });
});

describe('stale-selection reconciliation (#4281)', () => {
  const roster = [entry('izzie'), entry('cto-assistant')];

  it('reconcileSelectedAssistant_keeps_a_live_selection', () => {
    expect(reconcileSelectedAssistant('izzie', roster)).toBe('izzie');
  });

  it('reconcileSelectedAssistant_drops_a_stale_selection_to_concierge', () => {
    expect(reconcileSelectedAssistant('deleted-assistant', roster)).toBeNull();
  });

  it('reconcileSelectedAssistant_keeps_selection_while_roster_is_empty', () => {
    // Empty means "catalog not loaded / API unreachable", never "gone" — the
    // cold-start fetch race in App.svelte would otherwise discard a valid
    // selection on every launch.
    expect(reconcileSelectedAssistant('izzie', [])).toBe('izzie');
  });

  it('reconcileSelectedAssistant_passes_concierge_through', () => {
    expect(reconcileSelectedAssistant(null, roster)).toBeNull();
    expect(reconcileSelectedAssistant(null, [])).toBeNull();
  });
});
