/**
 * Tests for the Overview-card activation rules (#6370).
 *
 * Run: `node --test src/cardActions.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { cardActivation, isActivationKey } from './cardActions.js';

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
