/**
 * The Last Used column, shared by the Search and Memory tabs (#6424).
 *
 * Why: both daemons now report a per-entry `last_used_unix`, and both rosters
 * render and sort it the same way. One module keeps the two tabs from
 * disagreeing about what a missing timestamp means — which is the whole hazard
 * here, because "never used" and "used at the unix epoch" look identical once
 * either is turned into a date. Absent stays absent: it renders as a dash and
 * sorts last in BOTH directions, so a fleet of pre-feature rows never crowds
 * out the entries the operator is looking for.
 *
 * What: three pure functions over a unix-seconds number or null. No fetch, no
 * DOM, and `now` is a parameter so the relative-date output is testable.
 * Test: `lastUsed.test.js` — run `node --test src/lastUsed.test.js` from
 * `crates/trusty-console/ui`.
 */

/** What the cell shows when an entry has never been used. */
export const NEVER = '—';

/** Hover text for a [`NEVER`] cell, so the dash is not a mystery. */
export const NEVER_TITLE = 'Never used since this daemon started recording.';

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * Read an entry's timestamp, rejecting everything that is not a real one.
 *
 * A daemon that predates the field sends nothing; one that cannot read a
 * palace's stamp sends null. Both are "no timestamp". A non-number, or a zero,
 * is treated the same way — zero is the epoch, which is never a real answer
 * here and would otherwise sort as the oldest entry rather than as no entry.
 */
export function lastUsedAt(row) {
  const value = row?.last_used_unix;
  return typeof value === 'number' && Number.isFinite(value) && value > 0
    ? value
    : null;
}

/**
 * The text for one Last Used cell.
 *
 * Recent timestamps read relative, because that is the question an operator is
 * actually asking of this column ("is anything still using this?"). Past a
 * month the relative form stops being informative and it switches to a short
 * absolute date. `now` is unix seconds, injected so the output is deterministic
 * under test.
 */
export function formatLastUsed(row, now = Math.floor(Date.now() / 1000)) {
  const at = lastUsedAt(row);
  if (at === null) return NEVER;

  const age = now - at;
  if (age < 0) return 'just now'; // clock skew between daemon and browser
  if (age < MINUTE) return 'just now';
  if (age < HOUR) return `${Math.floor(age / MINUTE)}m ago`;
  if (age < DAY) return `${Math.floor(age / HOUR)}h ago`;
  if (age < 30 * DAY) return `${Math.floor(age / DAY)}d ago`;
  return new Date(at * 1000).toISOString().slice(0, 10);
}

/** Full timestamp for the cell's `title`, or the never-used explanation. */
export function lastUsedTitle(row) {
  const at = lastUsedAt(row);
  return at === null ? NEVER_TITLE : new Date(at * 1000).toISOString();
}

/**
 * Order rows by last use, newest or oldest first, with never-used rows last.
 *
 * Never-used rows sort last in BOTH directions on purpose. Sorting them first
 * under "oldest first" is defensible arithmetic and useless in practice: on any
 * existing daemon every pre-feature row is never-used, so it would fill the
 * whole first page with the rows carrying no information.
 *
 * `direction` is `'desc'` (most recent first) or `'asc'`. Returns a new array;
 * the input is not mutated, and rows that tie keep their original relative
 * order.
 */
export function sortByLastUsed(rows, direction = 'desc') {
  const sign = direction === 'asc' ? 1 : -1;
  return [...(rows ?? [])]
    .map((row, index) => ({ row, index }))
    .sort((a, b) => {
      const x = lastUsedAt(a.row);
      const y = lastUsedAt(b.row);
      if (x === null && y === null) return a.index - b.index;
      if (x === null) return 1;
      if (y === null) return -1;
      if (x === y) return a.index - b.index;
      return sign * (x - y);
    })
    .map((e) => e.row);
}

/**
 * The next state of a click-to-sort header: desc -> asc -> off.
 *
 * `null` restores the order the daemon sent, which is the only way back to it
 * once a sort has been applied.
 */
export function nextSortDirection(current) {
  if (current === 'desc') return 'asc';
  if (current === 'asc') return null;
  return 'desc';
}

/** The arrow a click-to-sort header shows for its current state. */
export function sortIndicator(current) {
  if (current === 'desc') return '↓';
  if (current === 'asc') return '↑';
  return '↕';
}
