// Verify selectable instances, internal-helper exclusion, and persisted selection.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import { get } from 'svelte/store';
import AssistantPicker from './AssistantPicker.svelte';
import { activeAgentId, catalogAgents, overlayAgents } from '../stores/app';
import { CONCIERGE_AGENT_ID } from '../lib/roster';
import { SELECTED_ASSISTANT_KEY } from '../lib/selectedAssistant';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;
let storage: Map<string, string>;

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

/** Every assistant card button, in render order. */
const cardButtons = () =>
  Array.from(target.querySelectorAll<HTMLButtonElement>('button[aria-pressed]'));

/** The card whose visible label starts with `label`. */
function card(label: string): HTMLButtonElement {
  const found = cardButtons().find((b) => (b.textContent ?? '').includes(label));
  if (!found) {
    throw new Error(
      `no card for ${label}: ${cardButtons().map((b) => b.textContent?.trim())}`,
    );
  }
  return found;
}

/** The catalog the stubbed `GET /api/agents` serves. */
const CATALOG = [
  { name: 'izzie', display_name: 'Izzie', description: 'Personal assistant' },
  { name: 'cto-assistant', display_name: 'CTO Bot' },
  // Stale catalogs can still return the internal helper.
  { name: CONCIERGE_AGENT_ID, display_name: 'Concierge' },
];

beforeEach(async () => {
  storage = installFakeLocalStorage();
  // Seeded THROUGH the fetch the component actually performs on mount, not by
  // writing the store behind it: the component re-drives `fetchAgentCatalog`
  // itself (the cold-start race), so a directly-seeded store would be
  // overwritten by that call and the fixture would silently vanish.
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify({ agents: CATALOG }), { status: 200 })),
  );
  overlayAgents.set([]);
  activeAgentId.set(null);
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(AssistantPicker, { target }) as unknown as Record<string, unknown>;
  // Let the mount-time catalog fetch settle, then flush the render it triggers.
  await vi.waitFor(() => expect(get(catalogAgents)).toHaveLength(CATALOG.length));
  flushSync();
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  catalogAgents.set([]);
  overlayAgents.set([]);
  activeAgentId.set(null);
  vi.unstubAllGlobals();
});

describe('AssistantPicker — the cards', () => {
  it('never offers Concierge even when the catalog includes ctrl', () => {
    expect(target.textContent).not.toContain('Concierge');
  });

  it('draws a card per assistant instance', () => {
    const labels = cardButtons().map((b) => b.textContent ?? '');
    expect(labels.some((l) => l.includes('Izzie'))).toBe(true);
    expect(labels.some((l) => l.includes('CTO Bot'))).toBe(true);
    expect(cardButtons()).toHaveLength(2);
  });

  it('offers a create action that reuses the assistant-template flow', () => {
    expect(target.textContent).toContain('New assistant');
    expect(target.textContent).toContain('assistant template');
  });

  it('marks only an explicitly selected assistant', () => {
    expect(cardButtons().every((b) => b.getAttribute('aria-pressed') === 'false')).toBe(true);
    flushSync(() => activeAgentId.set('izzie'));
    expect(card('Izzie').getAttribute('aria-pressed')).toBe('true');
    expect(card('CTO Bot').getAttribute('aria-pressed')).toBe('false');
  });
});

describe('AssistantPicker — selection', () => {
  it('selects an instance by its dispatchable id', () => {
    flushSync(() => card('Izzie').click());
    expect(get(activeAgentId)).toBe('izzie');
  });

  // `App.svelte` moves to the chat view on this event, so a card that selected
  // silently would leave the user staring at the picker they just used.
  it('announces the chosen assistant so the shell can leave the picker', async () => {
    unmount(instance!);
    const seen: (string | null)[] = [];
    instance = mount(AssistantPicker, {
      target,
      // Legacy `createEventDispatcher` events reach `mount` through `events`,
      // not as DOM events on the target element.
      events: {
        select: (e: CustomEvent<{ id: string | null }>) => seen.push(e.detail.id),
      },
    }) as unknown as Record<string, unknown>;
    await vi.waitFor(() => expect(cardButtons().length).toBe(2));
    flushSync(() => card('Izzie').click());
    expect(seen).toEqual(['izzie']);
  });

  // #4281 is the persistence surface; the picker must not grow a second one.
  it('persists through the store alone, with no picker-owned save call', () => {
    flushSync(() => card('Izzie').click());
    expect(storage.get(SELECTED_ASSISTANT_KEY)).toBe('izzie');
    flushSync(() => card('CTO Bot').click());
    expect(storage.get(SELECTED_ASSISTANT_KEY)).toBe('cto-assistant');
    // Exactly one key: a picker-owned key would show up here.
    expect([...storage.keys()]).toEqual([SELECTED_ASSISTANT_KEY]);
  });

  // The other branch of the Node-version split: with no usable storage the
  // selection must still work in memory. Asserted explicitly rather than left
  // to whichever runtime happens to run the suite.
  it('still selects when storage is unavailable', () => {
    vi.stubGlobal('localStorage', undefined);
    flushSync(() => card('Izzie').click());
    expect(get(activeAgentId)).toBe('izzie');
  });
});
