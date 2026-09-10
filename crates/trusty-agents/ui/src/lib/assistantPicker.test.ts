// Assistant cards exclude the internal helper; legacy selection decoding remains compatible.
import { describe, expect, it } from 'vitest';
import {
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
  it('has no cards while the roster is empty', () => {
    expect(buildPickerCards([])).toEqual([]);
  });

  it('excludes Concierge even when an old roster contains ctrl', () => {
    const cards = buildPickerCards([
      entry({ id: CONCIERGE_AGENT_ID, label: 'Concierge' }),
      entry(),
    ]);
    expect(cards.map((c) => c.id)).toEqual(['izzie']);
  });

  it('preserves the roster order the merge already established', () => {
    const cards = buildPickerCards([
      entry({ id: 'cto-assistant', label: 'CTO Bot' }),
      entry({ id: 'izzie', label: 'Izzie' }),
    ]);
    expect(cards.map((c) => c.id)).toEqual(['cto-assistant', 'izzie']);
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
    expect(cards[0].label).toBe('Izzie');
    expect(cards[0].description).toBe('Weather etc.');
  });
});

describe('decodeAssistantSelection — persistence compatibility', () => {
  it('maps the legacy ctrl sentinel back to null', () => {
    expect(decodeAssistantSelection(CONCIERGE_AGENT_ID)).toBeNull();
  });

  it('passes selectable instance ids through verbatim', () => {
    const cards = buildPickerCards([entry(), entry({ id: 'cto-assistant' })]);
    expect(cards.map((c) => decodeAssistantSelection(c.id))).toEqual(['izzie', 'cto-assistant']);
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
    // construction. This pins that these example identities remain visually distinct.
    const hues = ['ctrl', 'izzie', 'cto-assistant'].map(avatarHue);
    expect(new Set(hues).size).toBe(3);
  });
});
