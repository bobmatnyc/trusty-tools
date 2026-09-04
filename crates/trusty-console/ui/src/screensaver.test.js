/**
 * Tests for the screensaver route's decision layer (#6519, #6828).
 *
 * Run: `node --test src/screensaver.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 *
 * #6828 added the rotation's remount coverage. Two of its assertions read
 * `Screensaver.svelte` as TEXT, which is the instrument `bodyOverflow.test.js`
 * already uses here: which clock feeds the frame index is a decision the
 * component makes, and no node test can mount a Svelte 5 client component to
 * observe it.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  BACKOFF_AFTER_FAILURES,
  POLL_BASE_MS,
  POLL_CAP_MS,
  ROTATE_MS,
  enteredFromIdle,
  idleExpiredAt,
  isScreensaverPath,
  msUntilNextRotation,
  nextPollDelayMs,
  parseIdleMinutes,
  rotationIndexAt,
} from './screensaver.js';

const MINUTE_MS = 60_000;
/** The route's two frames: host cards, then the service list. */
const FRAME_COUNT = 2;
const SRC_DIR = dirname(fileURLToPath(import.meta.url));
const SAVER_SOURCE = readFileSync(join(SRC_DIR, 'Screensaver.svelte'), 'utf8');

/**
 * One page's rotation over a fake clock, driven the way Screensaver.svelte
 * drives it: mount reads the frame from the wall clock and arms a timer for the
 * remainder of the current frame, and each firing re-reads both. Nothing it
 * reports depends on when the page mounted, which is the property #6828 is
 * about.
 */
function mountRotation(clock) {
  let frame = rotationIndexAt(clock.nowMs, ROTATE_MS, FRAME_COUNT);
  let dueAtMs = clock.nowMs + msUntilNextRotation(clock.nowMs, ROTATE_MS);
  return {
    /** Fire the timer if the clock has passed it, then report what is on screen. */
    frame() {
      if (clock.nowMs >= dueAtMs) {
        frame = rotationIndexAt(clock.nowMs, ROTATE_MS, FRAME_COUNT);
        dueAtMs = clock.nowMs + msUntilNextRotation(clock.nowMs, ROTATE_MS);
      }
      return frame;
    },
  };
}

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

test('msUntilNextRotation waits out the remainder of the current frame', () => {
  assert.equal(msUntilNextRotation(0, ROTATE_MS), ROTATE_MS);
  // On a boundary the frame has only just started, so the wait is a whole
  // period. Zero would spin the timer.
  assert.equal(msUntilNextRotation(ROTATE_MS, ROTATE_MS), ROTATE_MS);
  assert.equal(msUntilNextRotation(ROTATE_MS + 1, ROTATE_MS), ROTATE_MS - 1);
  assert.equal(msUntilNextRotation(3 * ROTATE_MS - 5_000, ROTATE_MS), 5_000);
  for (let ms = 0; ms < 3 * ROTATE_MS; ms += 1_000) {
    const wait = msUntilNextRotation(ms, ROTATE_MS);
    assert.ok(wait > 0 && wait <= ROTATE_MS, `wait ${wait}ms out of range at ${ms}ms`);
  }
  // Unusable input falls back to a period, never to a zero delay.
  assert.equal(msUntilNextRotation(Number.NaN, ROTATE_MS), ROTATE_MS);
  assert.equal(msUntilNextRotation(-1, ROTATE_MS), ROTATE_MS);
  assert.equal(msUntilNextRotation(1_000, 0), ROTATE_MS - 1_000);
});

test('a page remounted every 5s across 25s still reaches the services frame', () => {
  // #6828: System Settings' Preview host rebuilt the WKWebView five times in
  // 33 s. A frame counted from MOUNT can never leave 0 there, because every
  // page dies at 5 s and the first transition was due at 20 s.
  const REMOUNT_MS = 5_000;
  const startMs = 2 * ROTATE_MS; // a frame-0 boundary: frame 1 starts 20s in
  const clock = { nowMs: startMs };
  const seen = new Set();
  let page;
  for (let elapsedMs = 0; elapsedMs <= 25_000; elapsedMs += 1_000) {
    clock.nowMs = startMs + elapsedMs;
    // The host throws the page away and builds a new one from scratch.
    if (elapsedMs % REMOUNT_MS === 0) page = mountRotation(clock);
    seen.add(page.frame());
  }
  assert.deepEqual([...seen].sort(), [0, 1], `frames seen: ${[...seen]}`);

  // And the mount that lands past the boundary shows the services frame on its
  // FIRST paint, rather than 20s after itself.
  clock.nowMs = startMs + ROTATE_MS;
  assert.equal(mountRotation(clock).frame(), 1);
});

test('Screensaver.svelte reads the rotation from the wall clock, not from mount', () => {
  // The frame index is pure and covered above; WHICH clock feeds it is the
  // component's decision, and #6828 was entirely that decision. `assert.ok` and
  // not `assert.match`, because a failing `match` prints the whole 500-line
  // component and buries what it was looking for.
  assert.ok(
    !/startedAt/.test(SAVER_SOURCE),
    'the rotation must not measure elapsed time from mount (#6828)',
  );
  assert.ok(
    /rotationIndexAt\(rotateNow, ROTATE_MS, FRAME_COUNT\)/.test(SAVER_SOURCE),
    'the frame must be read from the wall-clock `rotateNow` (#6828)',
  );
  assert.ok(
    /msUntilNextRotation\(Date\.now\(\), ROTATE_MS\)/.test(SAVER_SOURCE),
    'the rotation timer must wait to the next schedule boundary (#6828)',
  );
});

test('Screensaver.svelte reads the roster on mount, not only on the first poll', () => {
  // #6828: a fresh mount can now land directly on the services frame, so the
  // roster has to be in flight from mount. Deferring the first read to the 15s
  // poll interval would leave that frame empty for the whole 20s it is up.
  const onMountBody = SAVER_SOURCE.slice(SAVER_SOURCE.indexOf('onMount(() => {'));
  assert.ok(/\n {4}poll\(\);\n/.test(onMountBody), 'onMount must poll immediately');
  assert.ok(
    /Promise\.all\(\[fetchMachineStatus\(\), fetchServices\(\)\]\)/.test(SAVER_SOURCE),
    'one poll must read the host status and the service roster together',
  );
});
