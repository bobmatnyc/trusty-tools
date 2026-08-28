/**
 * Tests for the Memory tab's per-palace row rendering (#6372).
 *
 * Run: `node --test src/palaceRows.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { UNKNOWN, countCell, sourceBadge, statsSource } from './palaceRows.js';

test('a palace counted off disk shows its real numbers, not a dash', () => {
  // The defect this prevents (#6372): the tab printed `—` for every row whose
  // `cached` was false, so 92 populated palaces read as empty.
  const disk = {
    id: 'duetto',
    cached: false,
    stats_source: 'disk',
    drawer_count: 412,
    vector_count: 400,
    room_count: 6,
    kg_triple_count: 1200,
  };
  assert.equal(countCell(disk, 'drawer_count'), '412');
  assert.equal(countCell(disk, 'vector_count'), '400');
  assert.equal(countCell(disk, 'room_count'), '6');
  assert.equal(countCell(disk, 'kg_triple_count'), '1200');
});

test('a zero count is a zero, and an unreadable count is a dash', () => {
  const empty = { stats_source: 'disk', drawer_count: 0 };
  assert.equal(countCell(empty, 'drawer_count'), '0');

  const unreadable = { stats_source: 'unavailable', drawer_count: null };
  assert.equal(countCell(unreadable, 'drawer_count'), UNKNOWN);
});

test('rooms are a column the row can render', () => {
  // Rooms were absent from the payload entirely before #6372.
  assert.equal(countCell({ stats_source: 'cache', room_count: 3 }, 'room_count'), '3');
  assert.equal(countCell({ stats_source: 'cache' }, 'room_count'), UNKNOWN);
});

test('a pre-schema-3 daemon still renders its uncached rows as unknown', () => {
  // A console newer than its trusty-memory sees no `stats_source`. Reading the
  // old `cached: false` (which really did carry no counts) as `disk` would
  // print that daemon's placeholder zeros as if they were measurements.
  const legacy = { cached: false, drawer_count: 0 };
  assert.equal(statsSource(legacy), 'unavailable');
  assert.equal(sourceBadge(legacy).label, 'unreadable');

  const legacyCached = { cached: true, drawer_count: 7 };
  assert.equal(statsSource(legacyCached), 'cache');
  assert.equal(sourceBadge(legacyCached), null);
});

test('only the unreadable row carries a warning, and it quotes the daemon', () => {
  assert.equal(sourceBadge({ stats_source: 'cache' }), null);
  assert.equal(sourceBadge({ stats_source: 'disk' }).label, 'on disk');

  const badge = sourceBadge({
    stats_source: 'unavailable',
    stats_error: 'kg.redb is not readable: Database already open.',
  });
  assert.equal(badge.label, 'unreadable');
  assert.ok(
    badge.title.includes('kg.redb'),
    `the badge must carry the daemon's own reason: ${badge.title}`,
  );
});

test('a missing entry never claims to be counted', () => {
  assert.equal(statsSource(undefined), 'unavailable');
  assert.equal(countCell(undefined, 'drawer_count'), UNKNOWN);
});
