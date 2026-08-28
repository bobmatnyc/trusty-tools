/**
 * Tests for the stale-registration prune and palace-compact helpers (#6371).
 *
 * Run: `node --test src/cleanupFlow.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  censusSummary,
  compactConfirmMessage,
  compactUrl,
  pruneConfirmMessage,
  readCompactResult,
  readPruneResult,
  selectableOrphans,
  unjudgedRows,
} from './cleanupFlow.js';

const census = {
  orphans: [
    { id: 'tmp-a', root_path: '/private/var/folders/x/T/tmp-a' },
    { id: 'tmp-b', root_path: '/private/var/folders/x/T/tmp-b' },
  ],
  indeterminate: [
    { id: 'ext-1', root_path: '/Volumes/Kemono/project', reason: 'on an external volume' },
  ],
  live_count: 7,
  total: 10,
};

test('only the daemon-confirmed orphans are offered for deletion', () => {
  // The defect this prevents: a root the daemon could not CHECK is not a root
  // that is GONE. Offering the indeterminate list would delete a volume's whole
  // index roster the moment it was unplugged.
  const ids = selectableOrphans(census).map((r) => r.id);
  assert.deepEqual(ids, ['tmp-a', 'tmp-b']);
  assert.ok(!ids.includes('ext-1'), 'an unjudged root must never be selectable');
});

test('the unjudged rows are still reported, with the reason', () => {
  const rows = unjudgedRows(census);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].id, 'ext-1');
  assert.ok(rows[0].reason.length > 0, 'an unjudged row must say why');
});

test('a malformed or absent census offers nothing rather than throwing', () => {
  for (const bad of [null, undefined, {}, { orphans: 'all of them' }]) {
    assert.deepEqual(selectableOrphans(bad), [], JSON.stringify(bad));
    assert.deepEqual(unjudgedRows(bad), [], JSON.stringify(bad));
  }
});

test('an orphan row with no id is not a deletion candidate', () => {
  assert.deepEqual(selectableOrphans({ orphans: [{ root_path: '/gone' }, { id: '' }] }), []);
});

test('the census summary names both counts and the total', () => {
  const summary = censusSummary(census);
  assert.ok(summary.includes('2 stale'), summary);
  assert.ok(summary.includes('10'), summary);
  assert.ok(summary.includes('could not be checked'), summary);
});

test('a clean census says so without naming an unjudged count', () => {
  const summary = censusSummary({ orphans: [], indeterminate: [], total: 12 });
  assert.ok(summary.includes('No stale registrations'), summary);
  assert.ok(!summary.includes('could not be checked'), summary);
});

test('the confirm step names the count and the fate of the data', () => {
  const withData = pruneConfirmMessage(['a', 'b', 'c'], true);
  assert.ok(withData.includes('3'), withData);
  assert.ok(withData.includes('will be deleted too'), withData);

  const withoutData = pruneConfirmMessage(['a'], false);
  assert.ok(withoutData.includes('1 stale registration?'), withoutData);
  assert.ok(withoutData.includes('left in place'), withoutData);
});

test('a fully successful prune reads as success and keeps its rows', () => {
  const out = readPruneResult(200, {
    ok: true,
    removed: 2,
    failed: 0,
    results: [
      { id: 'tmp-a', ok: true },
      { id: 'tmp-b', ok: true },
    ],
  });
  assert.equal(out.ok, true);
  assert.ok(out.message.includes('2'), out.message);
  assert.equal(out.rows.length, 2);
});

test('a partial batch never reads as cleaned, and every row survives', () => {
  // The defect this prevents: one refused delete inside a batch reported as a
  // successful cleanup. The operator must see WHICH id failed and why.
  const out = readPruneResult(409, {
    ok: false,
    removed: 1,
    failed: 1,
    results: [
      { id: 'tmp-a', ok: true },
      {
        id: 'tmp-b',
        ok: false,
        error: "trusty-search skipped the delete: no registration for 'tmp-b' was removed",
      },
    ],
  });
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('1 could not be removed'), out.message);
  assert.equal(out.rows.length, 2, 'both rows must reach the operator');
  assert.equal(out.rows[1].ok, false);
  assert.ok(out.rows[1].error.includes('skipped the delete'), out.rows[1].error);
});

test('ok:true on a body with no rows is not a successful prune', () => {
  const out = readPruneResult(200, { ok: true, removed: 0, failed: 0, results: [] });
  assert.equal(out.ok, false, 'a prune that removed nothing removed nothing');
});

test('an unreachable daemon reads as failure with the console message', () => {
  const out = readPruneResult(503, {
    ok: false,
    error: 'trusty-search is not reachable: the console has no live address for it',
  });
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('not reachable'), out.message);
  assert.deepEqual(out.rows, []);
});

test('an unparseable prune body still produces a usable failure message', () => {
  const out = readPruneResult(500, null);
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('500'), out.message);
});

test('the compact URL targets the palace route and encodes the id', () => {
  assert.equal(compactUrl('scratch'), '/api/console/memory/palaces/scratch/compact');
  assert.equal(
    compactUrl('a/b'),
    '/api/console/memory/palaces/a%2Fb/compact',
    'a separator in an id must not become a path segment',
  );
});

test('the compact confirm step names the exact palace', () => {
  const msg = compactConfirmMessage('prod-corpus');
  assert.ok(msg.includes('prod-corpus'), msg);
  assert.notEqual(compactConfirmMessage('alpha'), compactConfirmMessage('beta'));
});

test('a confirmed compaction reports what it reclaimed', () => {
  const out = readCompactResult(200, {
    ok: true,
    id: 'scratch',
    detail: { orphans_removed: 7, total_checked: 120 },
  });
  assert.equal(out.ok, true);
  assert.ok(out.message.includes('7'), out.message);
  assert.ok(out.message.includes('120'), out.message);
});

test('a compaction with no counts still reads as success', () => {
  const out = readCompactResult(200, { ok: true, id: 'scratch', detail: {} });
  assert.equal(out.ok, true);
  assert.ok(out.message.includes('scratch'), out.message);
});

test('an unconfirmed compaction reads as failure carrying the daemon message', () => {
  const out = readCompactResult(409, {
    ok: false,
    id: 'scratch',
    error: "trusty-memory answered palace_compact without confirming it compacted 'scratch'",
  });
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('without confirming'), out.message);
});

test('ok:false on a 200 compaction still reads as failure', () => {
  const out = readCompactResult(200, { ok: false, id: 'x', error: 'nothing was compacted' });
  assert.equal(out.ok, false, 'the ok field decides, not the status code');
  assert.equal(out.message, 'nothing was compacted');
});
