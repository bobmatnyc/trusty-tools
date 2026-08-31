/**
 * Tests for the service-card status wording (#6416).
 *
 * Run: `node --test src/statusPresentation.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { cardPresentation, isOnDemand } from './statusPresentation.js';

const onDemand = (over = {}) => ({
  id: 'trusty-review',
  status: 'available',
  lifecycle: 'on_demand',
  ...over,
});

test('an installed on-demand tool shows no hint at all', () => {
  // The defect: the Trusty Review and Trusty Analyze cards read "Binary found
  // but daemon is not running" over a service that has no daemon to run. The
  // owner then ruled the follow-up explanatory sentence unnecessary too
  // (#6416) — the card carries no hint, same as a running daemon.
  const out = cardPresentation(onDemand());
  assert.equal(out.hint, null);
});

test('an installed on-demand tool is not painted as a warning', () => {
  // Amber is the console's "something needs attention" color. Resting is the
  // healthy state here, so it gets the same color a running daemon gets.
  const out = cardPresentation(onDemand());
  assert.equal(out.toneVar, 'var(--trusty-success)');
  assert.equal(out.label, 'Ready');
});

test('a stopped daemon keeps the sentence and the amber badge it had', () => {
  const out = cardPresentation({ id: 'trusty-search', status: 'available' });
  assert.equal(out.label, 'Available');
  assert.equal(out.toneVar, 'var(--trusty-warning)');
  assert.equal(out.hint.text, 'Binary found but daemon is not running.');
});

test('a payload with no lifecycle key reads as a daemon', () => {
  // Every payload written before #6416 omits the field.
  assert.equal(isOnDemand({ status: 'available' }), false);
  assert.equal(cardPresentation({ status: 'available' }).label, 'Available');
});

test('an on-demand tool that is not installed still says how to install it', () => {
  const out = cardPresentation(onDemand({ status: 'absent' }));
  assert.equal(out.hint.kind, 'install');
  assert.equal(out.hint.text, 'cargo install trusty-review');
});

test('a running service shows no hint at all', () => {
  assert.equal(cardPresentation({ id: 'trusty-memory', status: 'running' }).hint, null);
});

test('a degraded card carries the reason the daemon gave', () => {
  const out = cardPresentation({ id: 'trusty-mpm', status: 'degraded', hint: 'no metrics tool' });
  assert.equal(out.hint.kind, 'degraded');
  assert.equal(out.hint.text, 'no metrics tool');
  assert.equal(out.toneVar, 'var(--trusty-status-degraded)');
});

test('an unknown status renders verbatim rather than being guessed at', () => {
  const out = cardPresentation({ id: 'x', status: 'wedged' });
  assert.equal(out.label, 'wedged');
  assert.equal(out.hint, null);
});
