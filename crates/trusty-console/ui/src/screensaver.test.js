/**
 * Tests for the screensaver route's decision layer (#6519).
 *
 * Run: `node --test src/screensaver.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  BACKOFF_AFTER_FAILURES,
  POLL_BASE_MS,
  POLL_CAP_MS,
  ROTATE_MS,
  enteredFromIdle,
  idleExpiredAt,
  isScreensaverPath,
  nextPollDelayMs,
  parseIdleMinutes,
  rotationIndexAt,
} from './screensaver.js';

const MINUTE_MS = 60_000;

test('isScreensaverPath matches both routes the server serves', () => {
  // Both are real: /ui/screensaver falls through the SPA asset handler, and
  // /screensaver is its own route in console_ui.rs.
  assert.equal(isScreensaverPath('/ui/screensaver'), true);
  assert.equal(isScreensaverPath('/screensaver'), true);
  assert.equal(isScreensaverPath('/ui/screensaver/'), true);
  assert.equal(isScreensaverPath('/UI/Screensaver'), true);
});

test('isScreensaverPath rejects the console and near-miss paths', () => {
  assert.equal(isScreensaverPath('/ui'), false);
  assert.equal(isScreensaverPath('/'), false);
  // A segment that merely starts with the word is a different page; claiming it
  // would mount the screensaver over it with no way back.
  assert.equal(isScreensaverPath('/ui/screensaver-settings'), false);
  assert.equal(isScreensaverPath(undefined), false);
});

test('enteredFromIdle reads the idle marker only', () => {
  assert.equal(enteredFromIdle('?idle=1'), true);
  assert.equal(enteredFromIdle(''), false);
  assert.equal(enteredFromIdle('?idle=0'), false);
  assert.equal(enteredFromIdle(null), false);
});

test('rotationIndexAt advances exactly on the interval boundary', () => {
  assert.equal(rotationIndexAt(0, ROTATE_MS, 2), 0);
  assert.equal(rotationIndexAt(ROTATE_MS - 1, ROTATE_MS, 2), 0);
  assert.equal(rotationIndexAt(ROTATE_MS, ROTATE_MS, 2), 1);
  assert.equal(rotationIndexAt(2 * ROTATE_MS, ROTATE_MS, 2), 0);
  // A long sleep resumes on the frame the clock names, not the next one.
  assert.equal(rotationIndexAt(9 * ROTATE_MS + 5, ROTATE_MS, 2), 1);
});

test('rotationIndexAt yields frame 0 for every unusable input', () => {
  assert.equal(rotationIndexAt(ROTATE_MS, ROTATE_MS, 0), 0);
  assert.equal(rotationIndexAt(ROTATE_MS, 0, 2), 0);
  assert.equal(rotationIndexAt(-1, ROTATE_MS, 2), 0);
  assert.equal(rotationIndexAt(Number.NaN, ROTATE_MS, 2), 0);
  assert.equal(rotationIndexAt(ROTATE_MS, ROTATE_MS, 1), 0);
});

test('idleExpiredAt fires at the threshold, not before', () => {
  const now = 1_000_000;
  assert.equal(idleExpiredAt(now - 5 * MINUTE_MS + 1, now, 5), false);
  assert.equal(idleExpiredAt(now - 5 * MINUTE_MS, now, 5), true);
  assert.equal(idleExpiredAt(now - 60 * MINUTE_MS, now, 5), true);
});

test('idleExpiredAt is off unless a positive threshold is configured', () => {
  const now = 1_000_000;
  // The default state: no key, no automatic entry, however long the idle.
  assert.equal(idleExpiredAt(0, now, 0), false);
  assert.equal(idleExpiredAt(0, now, -5), false);
  assert.equal(idleExpiredAt(0, now, undefined), false);
  // A clock step backwards reads as no idle time, not as a huge one.
  assert.equal(idleExpiredAt(now + MINUTE_MS, now, 1), false);
});

test('nextPollDelayMs holds the base cadence through the tolerated misses', () => {
  for (let failures = 0; failures < BACKOFF_AFTER_FAILURES; failures += 1) {
    assert.equal(nextPollDelayMs(failures), POLL_BASE_MS);
  }
});

test('nextPollDelayMs doubles past the threshold and stops at the cap', () => {
  assert.equal(nextPollDelayMs(3), 30_000);
  assert.equal(nextPollDelayMs(4), 60_000);
  assert.equal(nextPollDelayMs(5), POLL_CAP_MS);
  assert.equal(nextPollDelayMs(50), POLL_CAP_MS);
  // Explicit base/cap, so the caller's own cadence is what backs off.
  assert.equal(nextPollDelayMs(3, 1000, 4000), 2000);
  assert.equal(nextPollDelayMs(9, 1000, 4000), 4000);
});

test('parseIdleMinutes treats every unusable value as off', () => {
  assert.equal(parseIdleMinutes('10'), 10);
  assert.equal(parseIdleMinutes('2.5'), 2.5);
  assert.equal(parseIdleMinutes('0'), 0);
  assert.equal(parseIdleMinutes('-3'), 0);
  assert.equal(parseIdleMinutes('ten'), 0);
  assert.equal(parseIdleMinutes(null), 0);
});
