// Assistant-picker card model (#4404 — the assistant picker as the default
// view; milestone M2).
//
// Why: the landing view should let the user choose WHO they are working with
// before working, and "Assistant" is a TYPE — Izzie and the CTO Assistant are
// INSTANCES of it, each with its own persona and home. So the picker's cards are
// over instances plus the Concierge plus a create action, never over agent
// types. Two things make that a data problem rather than a markup problem, and
// both live here so they are testable without a DOM:
//
//  1. **Concierge is `null` on the selection axis but `ctrl` everywhere else.**
//     `activeAgentId === null` is the tools-ARMED base PM/ctrl dispatch path;
//     dispatching `ctrl` BY NAME routes through the tools-OFF persona path
//     (`handlers.rs::resolve_agent_for_chat`) and silently strips Concierge's
//     delegation capability. A card therefore has to carry the id `ctrl` — so it
//     matches the persisted vocabulary and the config surface (`configAgentName`)
//     — and be DECODED back to `null` before it is written to `activeAgentId`.
//     `decodeAssistantSelection` is that one decode, and it is the reason this
//     module exists rather than the component inlining `entry.id`.
//  2. **`ctrl` legitimately appears in the roster too.** Since #3819 it is a
//     normal `role=assistant`, non-hidden catalog entry, so a picker that
//     rendered the roster verbatim would draw Concierge twice — the exact defect
//     Bob's "Concierge appears exactly once" fix removed from `ChatHeader`.
//     `buildPickerCards` excludes it from the roster-driven rows for the same
//     reason and in the same way.
//
// Persistence is NOT this module's job and needs no call: #4281 made
// `activeAgentId` (`stores/app.ts`) write through to localStorage on every
// change, so selecting a card is `activeAgentId.set(decodeAssistantSelection(id))`
// and nothing else. There is deliberately no picker-owned storage key.
//
// Card ART is out of scope (owner decision: manual upload only, generation a
// fast-follow entangled with #4405's undecided model choice). `monogram` and
// `avatarHue` are a deterministic typographic stand-in, not generated art —
// see their doc comments.
//
// Test: `assistantPicker.test.ts`.

import {
  CONCIERGE_AGENT_ID,
  CONCIERGE_LABEL,
  type RosterEntry,
} from './roster';

/** One selectable card on the landing picker. */
export interface PickerCard {
  /**
   * The card's identity in the ROSTER vocabulary — `ctrl` for Concierge, the
   * instance id otherwise. Never written to `activeAgentId` directly; pass it
   * through `decodeAssistantSelection` first.
   */
  id: string;
  label: string;
  description?: string;
  /**
   * Why this identity exists, for the card's footer chip: `concierge` is the
   * always-available default; `catalog` is a project-configured instance;
   * `overlay` is a user's own personalization.
   */
  origin: 'concierge' | 'catalog' | 'overlay';
}

/**
 * Why: Concierge is not a roster row on this surface (see the module header —
 * rendering `ctrl` from the roster is the duplicate-card defect), but it IS the
 * app's actual default identity and must be pickable. Building it here rather
 * than in the component keeps the label and the id in one place, shared with
 * the decode below.
 * What: the pinned first card. Its `id` is `CONCIERGE_AGENT_ID`, matching the
 * persisted sentinel so a card id can be stored verbatim.
 * Test: `buildPickerCards_pins_concierge_first`.
 */
export const CONCIERGE_CARD: PickerCard = {
  id: CONCIERGE_AGENT_ID,
  label: CONCIERGE_LABEL,
  description:
    'The always-available default. Runs with tools and delegation armed, and hands work to the right assistant or specialist.',
  origin: 'concierge',
};

/**
 * Why: the picker shows instances, and the roster already merges the project
 * catalog with the user's personalization overlays and drops the nameless base
 * `assistant` template (`buildRoster`). The only thing left to do is remove
 * `ctrl` — which the roster legitimately contains and this surface represents
 * with its own pinned card — so that Concierge appears exactly once.
 * What: `[Concierge, ...roster minus ctrl]`, preserving the roster's own order
 * (catalog entries first, then overlays sorted by label). Pure: no store reads,
 * so the ordering is asserted directly in tests.
 * Test: `buildPickerCards_pins_concierge_first`,
 * `buildPickerCards_never_draws_concierge_twice`,
 * `buildPickerCards_preserves_roster_order`,
 * `buildPickerCards_marks_overlay_instances`.
 */
export function buildPickerCards(roster: RosterEntry[]): PickerCard[] {
  const instances: PickerCard[] = roster
    .filter((entry) => entry.id !== CONCIERGE_AGENT_ID)
    .map((entry) => ({
      id: entry.id,
      label: entry.label,
      description: entry.description,
      origin: entry.source === 'overlay' ? 'overlay' : 'catalog',
    }));
  return [CONCIERGE_CARD, ...instances];
}

/**
 * Why: THE correctness guard of this surface. `activeAgentId` encodes Concierge
 * as `null`, and writing the literal `'ctrl'` instead would route every
 * subsequent message through the tools-OFF persona path
 * (`handlers.rs::resolve_agent_for_chat` special-cases nothing), silently
 * stripping the delegation capability that is the entire point of Concierge.
 * The card carries `ctrl` because the persisted vocabulary and the config
 * surface both address it that way; this is the single point at which that
 * vocabulary is translated back to the dispatch axis. It mirrors
 * `readSelectedAssistant`'s own `value === CONCIERGE_AGENT_ID ? null : value`,
 * on the write side.
 * What: `null` for the Concierge card id, the id verbatim otherwise.
 * Test: `decodeAssistantSelection_maps_ctrl_to_null`,
 * `decodeAssistantSelection_passes_an_instance_id_through`.
 */
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
