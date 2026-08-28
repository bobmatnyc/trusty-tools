/**
 * Overview-card activation rules (#6370).
 *
 * Why: an operator clicking anywhere on a service card expects to land on that
 * service, but a card offering two actions has no single destination to send
 * them to — making the whole surface clickable there would pick one action for
 * them. Deciding that from the action list, in one place, keeps ServiceCard
 * from growing a second idea of when a card is clickable, and lets the rule be
 * tested without a browser.
 *
 * What: two pure functions, no DOM and no Svelte.
 * Test: `cardActions.test.js` — run `node --test src/cardActions.test.js` from
 * `crates/trusty-console/ui`.
 */

/**
 * Does this key activate a `role="button"` element?
 *
 * Why: a div carrying `role="button"` gets none of a real button's keyboard
 * behavior — Enter and Space must be handled by hand or the card is
 * mouse-only. Space arrives as `' '` on every browser this console targets.
 *
 * @param {string} key A `KeyboardEvent.key` value.
 * @returns {boolean} True for Enter and Space, false for everything else.
 */
export function isActivationKey(key) {
  return key === 'Enter' || key === ' ';
}

/**
 * Decide how a card exposes its actions.
 *
 * Why: #6370 — one action means the whole card is the click target; more than
 * one means the card cannot stand for any of them and each keeps its own
 * button. Zero means the card is a static status tile and must NOT be
 * focusable, or keyboard users tab through cards that do nothing.
 *
 * @typedef {{ id: string, label: string, run: () => void }} CardAction
 * @param {CardAction[]} actions The actions this card offers, in display order.
 * @returns {{ mode: 'card' | 'buttons' | 'none', primary: CardAction | null }}
 *   `card` — the whole card activates `primary`; `buttons` — render one button
 *   per action and leave the card inert; `none` — no affordance at all.
 */
export function cardActivation(actions) {
  const list = Array.isArray(actions) ? actions : [];
  if (list.length === 1) return { mode: 'card', primary: list[0] };
  if (list.length === 0) return { mode: 'none', primary: null };
  return { mode: 'buttons', primary: null };
}
