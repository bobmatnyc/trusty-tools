/**
 * Tests for the Overview-card activation rules (#6370).
 *
 * Run: `node --test src/cardActions.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  cardActivation,
  cardDescribedBy,
  isActivationKey,
  sanitizeElementId,
} from './cardActions.js';

const action = (id) => ({ id, label: id, run: () => id });

test('a card with one action is clickable across its whole body', () => {
  // The defect this prevents: the pre-#6370 card responded only to the
  // "View details" button, so a click two pixels outside it did nothing.
  const out = cardActivation([action('details')]);
  assert.equal(out.mode, 'card');
  assert.equal(out.primary.id, 'details');
});

test('a card with two actions keeps discrete buttons and stays inert', () => {
  const out = cardActivation([action('details'), action('restart')]);
  assert.equal(out.mode, 'buttons');
  assert.equal(out.primary, null, 'no single action may stand for the card');
});

test('a card with no action is not focusable at all', () => {
  // A status-only tile that takes tab focus makes a keyboard user step through
  // cards that do nothing.
  const out = cardActivation([]);
  assert.equal(out.mode, 'none');
  assert.equal(out.primary, null);
});

test('a missing action list reads as no action, not a crash', () => {
  assert.equal(cardActivation(undefined).mode, 'none');
  assert.equal(cardActivation(null).mode, 'none');
});

test('Enter and Space activate a clickable card; other keys do not', () => {
  assert.equal(isActivationKey('Enter'), true);
  assert.equal(isActivationKey(' '), true);
  assert.equal(isActivationKey('Tab'), false);
  assert.equal(isActivationKey('Escape'), false);
  assert.equal(isActivationKey('a'), false);
  assert.equal(isActivationKey('Space'), false, 'the key value for space is a single space');
});

test('a degraded card is described by its status and its hint, not only its label', () => {
  // The defect this prevents: aria-label replaces the accessible name a card's
  // contents would compute, so without a description a screen-reader user hears
  // "Trusty Memory - View details" and never learns the card is degraded.
  const ids = cardDescribedBy({ id: 'trusty-memory', status: 'degraded' }).split(' ');
  assert.ok(ids.includes('svc-trusty-memory-status'), ids.join(' '));
  assert.ok(ids.includes('svc-trusty-memory-hint'), ids.join(' '));
  assert.equal(ids[0], 'svc-trusty-memory-status', 'the status is read first');
});

test('every card is described by its status, whatever the status is', () => {
  for (const status of ['running', 'degraded', 'available', 'absent']) {
    const ids = cardDescribedBy({ id: 'trusty-search', status }).split(' ');
    assert.ok(ids.includes('svc-trusty-search-status'), status + ': ' + ids.join(' '));
  }
});

test('the hint is described for exactly the statuses whose card renders one', () => {
  // ServiceCard renders a hint paragraph for absent/available/degraded only.
  // A running card has no hint to point at, so naming one would leave
  // aria-describedby referencing an element that does not exist.
  const hinted = (status) =>
    cardDescribedBy({ id: 'x', status }).split(' ').includes('svc-x-hint');
  assert.equal(hinted('absent'), true);
  assert.equal(hinted('available'), true);
  assert.equal(hinted('degraded'), true);
  assert.equal(hinted('running'), false);
});

test('the version is described only when the service reports one', () => {
  const withVersion = cardDescribedBy({ id: 'x', status: 'running', version: '0.8.0' });
  assert.ok(withVersion.split(' ').includes('svc-x-version'), withVersion);
  const without = cardDescribedBy({ id: 'x', status: 'running' });
  assert.ok(!without.split(' ').includes('svc-x-version'), without);
});

test('two distinct service ids produce two distinct described-by ids', () => {
  // Renamed from "two cards never share a described-by id" (PR #6373 review,
  // carried by #6371): that name claimed an injectivity the sanitizer does not
  // have, and this assertion never tested it — see the collision test below.
  // What it does prove is the case that matters for the real roster, whose ids
  // differ before sanitizing.
  assert.notEqual(
    cardDescribedBy({ id: 'trusty-search', status: 'running' }),
    cardDescribedBy({ id: 'trusty-memory', status: 'running' }),
  );
});

test('the sanitizer collides on ids that differ only in a disallowed character', () => {
  // The genuine collision case the renamed test above was believed to catch and
  // does not. It is pinned rather than fixed because these ids come from the
  // console's own fixed service roster, where every id is already distinct
  // within [a-z-]; a caller feeding this daemon- or operator-supplied ids would
  // need a different derivation, and this test is what says so out loud.
  assert.equal(sanitizeElementId('a/b'), 'a-b');
  assert.equal(sanitizeElementId('a b'), 'a-b');
  assert.equal(
    cardDescribedBy({ id: 'a/b', status: 'running' }),
    cardDescribedBy({ id: 'a b', status: 'running' }),
  );
});

test('an id carrying a separator still produces a usable element id', () => {
  assert.equal(
    cardDescribedBy({ id: 'weird/id here', status: 'running' }),
    'svc-weird-id-here-status',
  );
});

test('the described-by ids and the markup ids come from one sanitizer', () => {
  // The defect this prevents: ServiceCard used to inline the same regex, so a
  // change to one derivation left aria-describedby pointing at ids the markup
  // no longer emitted. Both now call sanitizeElementId, and this pins the two
  // to the same output.
  const id = 'weird/id here';
  assert.equal(
    cardDescribedBy({ id, status: 'running' }),
    `svc-${sanitizeElementId(id)}-status`,
  );
});

test('the sanitizer survives a missing id rather than throwing', () => {
  assert.equal(sanitizeElementId(undefined), '');
  assert.equal(sanitizeElementId(null), '');
});
