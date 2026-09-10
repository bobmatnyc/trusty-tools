// Selectable assistant cards exclude Concierge, the internal configuration helper.
// Keep the legacy ctrl decode compatible with persisted selections.
import { CONCIERGE_AGENT_ID, type RosterEntry } from './roster';

/** One selectable assistant instance on the landing picker. */
export interface PickerCard {
  id: string;
  label: string;
  description?: string;
  origin: 'catalog' | 'overlay';
}

/** Preserve roster order while defensively excluding the internal helper. */
export function buildPickerCards(roster: RosterEntry[]): PickerCard[] {
  return roster
    .filter((entry) => entry.id !== CONCIERGE_AGENT_ID)
    .map((entry) => ({
      id: entry.id,
      label: entry.label,
      description: entry.description,
      origin: entry.source === 'overlay' ? 'overlay' : 'catalog',
    }));
}

/** Decode the legacy persisted sentinel without changing internal dispatch. */
export function decodeAssistantSelection(cardId: string): string | null {
  return cardId === CONCIERGE_AGENT_ID ? null : cardId;
}

/**
 * Why (#4404, scoped): the issue asks for a logo per card, INFERRED from the
 * assistant with user override by upload. Generation is explicitly deferred —
 * the owner's decision is manual upload only for now, and the generation path
 * is entangled with #4405's undecided model choice — and no avatar field exists
 * anywhere in this data model yet. A card with no visual identity at all is
 * worse than one with a typographic identity, so cards carry initials. This is
 * a deterministic stand-in, NOT generated art, and it adds no dependency.
 * What: up to two initials from the label's first two words, uppercased;
 * falls back to the first two characters of a single-word label, and to `?` for
 * a label with no alphanumeric content (which `slugify` would also reject).
 * Test: `monogram_takes_initials_of_the_first_two_words`,
 * `monogram_falls_back_to_leading_characters`,
 * `monogram_handles_a_label_with_no_letters`.
 */
export function monogram(label: string): string {
  const words = label.trim().split(/\s+/).filter((w) => /[a-z0-9]/i.test(w));
  if (words.length === 0) return '?';
  if (words.length === 1) {
    return words[0].replace(/[^a-z0-9]/gi, '').slice(0, 2).toUpperCase() || '?';
  }
  return words
    .slice(0, 2)
    .map((w) => w.replace(/[^a-z0-9]/gi, '').charAt(0))
    .join('')
    .toUpperCase();
}

/**
 * Why: the monogram tiles need to be distinguishable at a glance, and a hue
 * derived from the id is stable across launches without storing anything — a
 * random or index-derived colour would change when the roster grows, which
 * makes the picker feel like a different app between sessions.
 * What: a deterministic hue in `[0, 360)` from a small FNV-style hash of the id.
 * Purely decorative: nothing branches on it, and it is never the sole carrier of
 * identity (the label is always rendered).
 * Test: `avatarHue_is_deterministic_and_in_range`,
 * `avatarHue_separates_common_ids`.
 */
export function avatarHue(id: string): number {
  let hash = 2166136261;
  for (let i = 0; i < id.length; i++) {
    hash ^= id.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return Math.abs(hash) % 360;
}
