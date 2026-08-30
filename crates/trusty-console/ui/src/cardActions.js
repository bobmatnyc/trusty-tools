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

import { cardPresentation } from './statusPresentation.js';

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

/**
 * Turn an arbitrary string into something usable as an HTML element id.
 *
 * Why: #6370 derived element ids from `service.id` in TWO places — here and in
 * `ServiceCard.svelte` — with the same regex written out twice. Two copies of
 * an id-derivation rule is how `aria-describedby` starts pointing at ids the
 * markup no longer emits: change one and the description silently references
 * nothing. This is the one rule; both read it.
 *
 * What: replaces every character outside `[A-Za-z0-9_-]` with `-`. It is NOT
 * injective — `a/b` and `a b` both become `a-b` — which is safe only because
 * these ids come from the console's own fixed service roster
 * (`detect::order_for_display`), where every id is already distinct within
 * `[a-z-]`. Do not reuse it for ids that arrive from a daemon or an operator.
 *
 * @param {unknown} value The raw id.
 * @returns {string} The sanitized id fragment.
 */
export function sanitizeElementId(value) {
  return String(value ?? '').replace(/[^A-Za-z0-9_-]/g, '-');
}

/**
 * Element ids that describe a card, for its `aria-describedby`.
 *
 * Why: #6370 — a clickable card takes its accessible NAME from `aria-label`,
 * which replaces the name the contents would otherwise compute. Without a
 * description, a screen-reader user tabbing across the Overview hears
 * "Trusty Memory — View details" and nothing about the card being degraded,
 * so the one card that needs attention sounds like the five that do not.
 * Pointing at the elements already on screen — rather than restating their
 * text in a hidden node — means the spoken description cannot drift from the
 * visible one.
 *
 * What: always the status badge; the version when the service reports one;
 * the hint paragraph when the card renders one. Whether it does is asked of
 * `cardPresentation` (#6416) rather than kept as a second list of statuses
 * here — a list that drifts is how a screen reader loses a sentence a sighted
 * user can see. Ids are derived from `service.id`, which is unique per card,
 * with anything outside `[A-Za-z0-9_-]` replaced so the result is a usable id.
 *
 * @param {{ id: string, status: string, version?: string, lifecycle?: string }} service
 * @returns {string} A space-separated id list, in reading order.
 */
export function cardDescribedBy(service) {
  const sid = sanitizeElementId(service?.id);
  const ids = [`svc-${sid}-status`];
  if (service?.version) ids.push(`svc-${sid}-version`);
  if (cardPresentation(service).hint) ids.push(`svc-${sid}-hint`);
  return ids.join(' ');
}
