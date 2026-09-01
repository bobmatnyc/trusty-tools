/**
 * The screensaver route's decision layer (#6519, phase 3 of #6516).
 *
 * Why: phase 4 (#6520) wraps this route in a macOS `.saver` bundle — a
 * WKWebView that loads `http://127.0.0.1:7788/ui/screensaver` and then receives
 * no input for hours. Everything that decides WHAT the screen shows at a given
 * moment therefore has to be correct without anyone watching it, which means it
 * has to be testable without a browser. This module holds those four decisions
 * as pure functions of time; `Screensaver.svelte` is a renderer over them.
 *
 * What: no DOM, no fetch, no timers. Every function takes the clock as an
 * argument and returns a value. The one impure helper, [`readIdleMinutes`], is
 * a try/catch wrapper around `localStorage` whose parsing half is pure and
 * separately tested — the same split `theme.svelte.js` uses for its own key.
 * Test: `screensaver.test.js` — run `node --test src/screensaver.test.js` from
 * `crates/trusty-console/ui`.
 */

/** Where the console SPA lives, and where any input on the screensaver returns. */
export const CONSOLE_URL = '/ui';

/**
 * The screensaver route.
 *
 * `/screensaver` serves the same shell (see `console_ui.rs`) and is what a
 * hand-typed URL is likeliest to be; this is the canonical form phase 4 loads.
 */
export const SCREENSAVER_URL = '/ui/screensaver';

/**
 * The idle-entry URL, carrying the marker that says nobody asked for this.
 *
 * The screensaver treats a gesture differently depending on how it was
 * reached — see [`enteredFromIdle`].
 */
export const IDLE_ENTRY_URL = `${SCREENSAVER_URL}?idle=1`;

/**
 * `localStorage` key holding the idle-entry threshold, in minutes.
 *
 * Absent or `0` means the console never navigates to the screensaver on its
 * own. There is no settings UI for this key in phase 3: automatic entry is
 * opt-in, and an operator who wants it sets the key by hand.
 */
export const IDLE_STORAGE_KEY = 'trusty-console-screensaver-idle-minutes';

/** Events that count as "someone is here". */
export const IDLE_EVENTS = ['mousemove', 'keydown', 'pointerdown'];

/** Default poll cadence — the same 15s the Overview dashboard uses. */
export const POLL_BASE_MS = 15_000;

/** Slowest the poll ever backs off to while the daemon is unreachable. */
export const POLL_CAP_MS = 60_000;

/**
 * Consecutive failures tolerated at full speed before backoff starts.
 *
 * Three misses is ~45s — long enough that a daemon restart or one dropped
 * response does not change the cadence at all.
 */
export const BACKOFF_AFTER_FAILURES = 3;

/** How long one rotation frame stays on screen. */
export const ROTATE_MS = 20_000;

/** True only for a real, finite number. */
function isNum(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

/**
 * Whether a pathname names the screensaver route.
 *
 * Matches a whole path SEGMENT, case-insensitively, so both `/screensaver` and
 * `/ui/screensaver` (with or without a trailing slash) resolve, while a path
 * that merely starts with the word — a future `/ui/screensaver-settings` — does
 * not. A substring test would claim that page too, and the claim is
 * unrecoverable: `main.js` mounts a different component and the real page never
 * renders.
 */
export function isScreensaverPath(pathname) {
  if (typeof pathname !== 'string') return false;
  return pathname
    .toLowerCase()
    .split('/')
    .some((segment) => segment === 'screensaver');
}

/**
 * Whether this view was reached by the idle timer rather than by request.
 *
 * The two entries want opposite things from the first gesture. Someone who
 * typed the URL wants fullscreen, which the browser only grants inside a user
 * gesture; someone whose console timed out wants their console back. The
 * marker in the query string is what separates them.
 */
export function enteredFromIdle(search) {
  if (typeof search !== 'string') return false;
  return new URLSearchParams(search).get('idle') === '1';
}

/**
 * Which rotation frame is showing after `elapsedMs` on the route.
 *
 * Derived from elapsed time rather than counted per tick so a missed timer —
 * a sleeping laptop, a throttled background tab — resumes on the frame the
 * clock says, not the one a lost tick would have left behind. Anything
 * unusable (no frames, a non-positive interval, a negative or non-finite
 * elapsed) yields frame 0, which always exists.
 */
export function rotationIndexAt(elapsedMs, intervalMs, frameCount) {
  if (!isNum(frameCount) || frameCount < 1) return 0;
  if (!isNum(intervalMs) || intervalMs <= 0) return 0;
  if (!isNum(elapsedMs) || elapsedMs < 0) return 0;
  return Math.floor(elapsedMs / intervalMs) % Math.floor(frameCount);
}

/**
 * Whether the console has been idle long enough to hand over to the screensaver.
 *
 * `idleMinutes` of zero, negative, or anything non-numeric means the feature is
 * off and this is always false — that is the default state, so the guard is the
 * common path, not an edge case. A `nowMs` behind `lastInputMs` (a clock step)
 * reads as no idle time rather than as an enormous negative one.
 */
export function idleExpiredAt(lastInputMs, nowMs, idleMinutes) {
  if (!isNum(idleMinutes) || idleMinutes <= 0) return false;
  if (!isNum(lastInputMs) || !isNum(nowMs)) return false;
  const idleMs = nowMs - lastInputMs;
  if (idleMs < 0) return false;
  return idleMs >= idleMinutes * 60_000;
}

/**
 * How long to wait before the next status poll.
 *
 * Holds the base cadence through [`BACKOFF_AFTER_FAILURES`] misses, then
 * doubles per further failure up to `capMs`. A screensaver left on an
 * unreachable daemon overnight otherwise issues thousands of futile requests;
 * one success resets the caller's counter and with it this delay.
 */
export function nextPollDelayMs(
  consecutiveFailures,
  baseMs = POLL_BASE_MS,
  capMs = POLL_CAP_MS,
) {
  if (!isNum(baseMs) || baseMs <= 0) return POLL_BASE_MS;
  if (!isNum(consecutiveFailures) || consecutiveFailures < BACKOFF_AFTER_FAILURES) {
    return baseMs;
  }
  const doublings = Math.floor(consecutiveFailures) - BACKOFF_AFTER_FAILURES + 1;
  const delay = baseMs * 2 ** doublings;
  return isNum(capMs) && capMs > 0 ? Math.min(delay, capMs) : delay;
}

/**
 * The idle threshold a stored value represents, in minutes; `0` means off.
 *
 * Everything unusable collapses to `0` rather than to a default: a typo in a
 * hand-edited key must leave the console alone, never arm a timer the operator
 * did not ask for.
 */
export function parseIdleMinutes(raw) {
  const value = Number.parseFloat(raw);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

/**
 * Read the idle threshold from `localStorage`; `0` when unset or unreadable.
 *
 * `localStorage` throws outright in some privacy modes, and this runs during
 * the console's mount — the same defensive read `theme.svelte.js` performs.
 */
export function readIdleMinutes() {
  try {
    return parseIdleMinutes(localStorage.getItem(IDLE_STORAGE_KEY));
  } catch (_) {
    return 0;
  }
}
