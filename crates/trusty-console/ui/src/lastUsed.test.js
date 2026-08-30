/**
 * Tests for the shared Last Used column (#6424).
 *
 * Run: `node --test src/lastUsed.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  NEVER,
  formatLastUsed,
  lastUsedAt,
  lastUsedTitle,
  nextSortDirection,
  sortByLastUsed,
  sortIndicator,
} from './lastUsed.js';

const NOW = 1_800_000_000;

test('a missing, null, or zero timestamp is all the same "never"', () => {
  // The defect this prevents: zero is the unix epoch, so a row carrying it
  // would render as 1970 and sort as the oldest entry in the table rather than
  // as an entry with no answer at all.
  assert.equal(lastUsedAt(undefined), null);
  assert.equal(lastUsedAt({}), null);
  assert.equal(lastUsedAt({ last_used_unix: null }), null);
  assert.equal(lastUsedAt({ last_used_unix: 0 }), null);
  assert.equal(lastUsedAt({ last_used_unix: 'yesterday' }), null);
  assert.equal(formatLastUsed({}, NOW), NEVER);
});

test('a real timestamp reads relative while it is recent', () => {
  const at = (age) => ({ last_used_unix: NOW - age });
  assert.equal(formatLastUsed(at(10), NOW), 'just now');
  assert.equal(formatLastUsed(at(5 * 60), NOW), '5m ago');
  assert.equal(formatLastUsed(at(3 * 3600), NOW), '3h ago');
  assert.equal(formatLastUsed(at(4 * 86400), NOW), '4d ago');
});

test('past a month it switches to a short absolute date', () => {
  const old = { last_used_unix: 1_700_000_000 };
  assert.equal(formatLastUsed(old, NOW), '2023-11-14');
});

test('a daemon clock ahead of the browser does not render a negative age', () => {
  assert.equal(formatLastUsed({ last_used_unix: NOW + 30 }, NOW), 'just now');
});

test('the title carries the full timestamp, or says why there is none', () => {
  assert.equal(
    lastUsedTitle({ last_used_unix: 1_700_000_000 }),
    '2023-11-14T22:13:20.000Z',
  );
  assert.match(lastUsedTitle({}), /Never used/);
});

test('descending sort puts the most recent first and never-used last', () => {
  const rows = [
    { id: 'never', last_used_unix: null },
    { id: 'old', last_used_unix: 100 },
    { id: 'new', last_used_unix: 900 },
    { id: 'mid', last_used_unix: 500 },
  ];
  assert.deepEqual(
    sortByLastUsed(rows, 'desc').map((r) => r.id),
    ['new', 'mid', 'old', 'never'],
  );
});

test('ascending sort ALSO puts never-used last, not first', () => {
  // The rule this pins: on any existing daemon every pre-feature row is
  // never-used, so sorting nulls first under "oldest first" would fill the
  // whole first page with rows carrying no information.
  const rows = [
    { id: 'never', last_used_unix: null },
    { id: 'new', last_used_unix: 900 },
    { id: 'old', last_used_unix: 100 },
    { id: 'also-never' },
  ];
  assert.deepEqual(
    sortByLastUsed(rows, 'asc').map((r) => r.id),
    ['old', 'new', 'never', 'also-never'],
  );
});

test('ties and never-used rows keep the order the daemon sent', () => {
  const rows = [
    { id: 'b', last_used_unix: 500 },
    { id: 'a', last_used_unix: 500 },
    { id: 'z' },
    { id: 'y' },
  ];
  assert.deepEqual(
    sortByLastUsed(rows, 'desc').map((r) => r.id),
    ['b', 'a', 'z', 'y'],
  );
});

test('sorting does not mutate the array it was given', () => {
  const rows = [{ id: 'a', last_used_unix: 1 }, { id: 'b', last_used_unix: 2 }];
  sortByLastUsed(rows, 'desc');
  assert.deepEqual(rows.map((r) => r.id), ['a', 'b']);
});

test('an empty or absent roster sorts to an empty array', () => {
  assert.deepEqual(sortByLastUsed([], 'desc'), []);
  assert.deepEqual(sortByLastUsed(undefined, 'desc'), []);
});

test('clicking the header cycles desc, asc, then back to daemon order', () => {
  assert.equal(nextSortDirection(null), 'desc');
  assert.equal(nextSortDirection('desc'), 'asc');
  assert.equal(nextSortDirection('asc'), null);
  assert.equal(sortIndicator(null), '↕');
  assert.equal(sortIndicator('desc'), '↓');
  assert.equal(sortIndicator('asc'), '↑');
});
