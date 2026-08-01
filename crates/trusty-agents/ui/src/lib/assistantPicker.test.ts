// Why (#4404): the picker's correctness is almost entirely in its DATA, and the
// one bug that would be invisible in the UI is the Concierge decode — a card
// that wrote `'ctrl'` onto `activeAgentId` would look identical on screen while
// silently routing every subsequent message through the tools-OFF persona path
// and stripping Concierge's delegation capability. The second is the duplicate:
// `ctrl` is a normal roster entry since #3819, so a picker that rendered the
// roster verbatim draws Concierge twice — the exact defect Bob's "Concierge
// appears exactly once" fix removed from the header switcher.
// What: pure unit tests over `buildPickerCards` / `decodeAssistantSelection` and
// the monogram/hue stand-ins. No DOM and no stores — the component wiring is
// covered by `components/AssistantPicker.test.ts`.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  CONCIERGE_CARD,
  avatarHue,
  buildPickerCards,
  decodeAssistantSelection,
  monogram,
} from './assistantPicker';
import { CONCIERGE_AGENT_ID, type RosterEntry } from './roster';

function entry(over: Partial<RosterEntry> = {}): RosterEntry {
  return {
    id: 'izzie',
    label: 'Izzie',
    description: 'Personal assistant',
    source: 'catalog',
    kind: 'assistant',
    ...over,
  };
}

describe('buildPickerCards', () => {
  it('pins Concierge first, ahead of every roster instance', () => {
    const cards = buildPickerCards([entry()]);
    expect(cards[0]).toEqual(CONCIERGE_CARD);
    expect(cards[0].id).toBe(CONCIERGE_AGENT_ID);
    expect(cards[0].origin).toBe('concierge');
  });

  it('offers Concierge even when the roster has not loaded', () => {
    // The cold-start case: the catalog fetch races the sidecar, so an empty
    // roster is "not loaded", never "no assistants exist". A picker with no
    // selectable card at all would be a dead landing view.
    expect(buildPickerCards([])).toHaveLength(1);
  });

  // The duplicate-card defect. `ctrl` is a legitimate role=assistant,
  // non-hidden catalog entry since #3819, so this is a live hazard, not a
  // hypothetical one.
  it('never draws Concierge twice when ctrl is in the roster', () => {
    const cards = buildPickerCards([
      entry({ id: CONCIERGE_AGENT_ID, label: 'Concierge' }),
      entry(),
    ]);
    expect(cards.filter((c) => c.id === CONCIERGE_AGENT_ID)).toHaveLength(1);
    expect(cards.map((c) => c.id)).toEqual([CONCIERGE_AGENT_ID, 'izzie']);
  });

  it('preserves the roster order the merge already established', () => {
    const cards = buildPickerCards([
      entry({ id: 'cto-assistant', label: 'CTO Bot' }),
      entry({ id: 'izzie', label: 'Izzie' }),
    ]);
    expect(cards.map((c) => c.id)).toEqual([CONCIERGE_AGENT_ID, 'cto-assistant', 'izzie']);
  });

  it("distinguishes a user's own overlay instance from a project one", () => {
    const cards = buildPickerCards([
      entry({ id: 'mine', source: 'overlay' }),
      entry({ id: 'theirs', source: 'catalog' }),
    ]);
    expect(cards.find((c) => c.id === 'mine')?.origin).toBe('overlay');
    expect(cards.find((c) => c.id === 'theirs')?.origin).toBe('catalog');
  });

  it('carries the roster label and description onto the card', () => {
    const cards = buildPickerCards([entry({ label: 'Izzie', description: 'Weather etc.' })]);
    expect(cards[1].label).toBe('Izzie');
    expect(cards[1].description).toBe('Weather etc.');
  });
});

describe('decodeAssistantSelection — the tools-armed guard', () => {
  // THE assertion of this file. `activeAgentId === null` is the tools-ARMED
  // ctrl path; the literal `'ctrl'` is the tools-OFF persona path. The card
  // must carry `ctrl` (it is the persisted sentinel and the config vocabulary)
  // and must never be written through unchanged.
  it('maps the Concierge card id back to null, never to the literal', () => {
    expect(decodeAssistantSelection(CONCIERGE_AGENT_ID)).toBeNull();
    expect(decodeAssistantSelection(CONCIERGE_CARD.id)).toBeNull();
  });

  it('passes an instance id through verbatim', () => {
    expect(decodeAssistantSelection('izzie')).toBe('izzie');
    expect(decodeAssistantSelection('cto-assistant')).toBe('cto-assistant');
  });

  it('is the exact inverse of the persisted encoding for every card', () => {
    // Round-trip property: every card the picker can draw decodes to a value
    // `activeAgentId` accepts, and only Concierge collapses to null.
    const cards = buildPickerCards([entry({ id: 'izzie' }), entry({ id: 'cto-assistant' })]);
    const decoded = cards.map((c) => decodeAssistantSelection(c.id));
    expect(decoded).toEqual([null, 'izzie', 'cto-assistant']);
  });
});

describe('monogram — the card-art stand-in', () => {
  it('takes the initials of the first two words', () => {
    expect(monogram('CTO Bot')).toBe('CB');
    expect(monogram('Chief Technology Officer')).toBe('CT');
  });

  it('falls back to leading characters for a single word', () => {
    expect(monogram('Izzie')).toBe('IZ');
    expect(monogram('X')).toBe('X');
  });

  it('never renders empty for a label with no letters or digits', () => {
    // `slugify` would reject these too; the picker must still draw a tile
    // rather than an empty box.
    expect(monogram('!!!')).toBe('?');
    expect(monogram('   ')).toBe('?');
  });

  it('ignores punctuation between words', () => {
    expect(monogram('Izzie — Assistant')).toBe('IA');
  });
});

describe('avatarHue', () => {
  it('is deterministic and inside the hue range', () => {
    for (const id of ['ctrl', 'izzie', 'cto-assistant', '']) {
      const hue = avatarHue(id);
      expect(hue).toBe(avatarHue(id));
      expect(hue).toBeGreaterThanOrEqual(0);
      expect(hue).toBeLessThan(360);
    }
  });

  it('separates the ids this app actually ships', () => {
    // Not a general collision guarantee — a 360-bucket hash has collisions by
    // construction. This pins that the three identities a user meets on a
    // default install are visually distinct.
    const hues = ['ctrl', 'izzie', 'cto-assistant'].map(avatarHue);
    expect(new Set(hues).size).toBe(3);
  });
});
