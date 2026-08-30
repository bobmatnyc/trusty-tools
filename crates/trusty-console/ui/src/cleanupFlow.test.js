/**
 * Tests for the stale-registration prune and palace-compact helpers (#6371).
 *
 * Run: `node --test src/cleanupFlow.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  PRUNE_DELETE_DATA_DEFAULT,
  censusSummary,
  readDeregisterResult,
  unjudgedConfirmMessage,
  unjudgedReviewNote,
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
    {
      id: 'ext-1',
      root_path: '/Volumes/Kemono/project',
      reason: 'on an external volume',
      colocated: false,
      repo_identity: null,
    },
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

test('#6422: the prune panel starts with the on-disk data going too', () => {
  // The owner ruling, on the batch surface. Against the pre-fix code the panel
  // opened with the checkbox unticked, so a confirmed prune cleared dozens of
  // registrations and reclaimed no disk at all.
  assert.equal(PRUNE_DELETE_DATA_DEFAULT, true);
  assert.ok(
    pruneConfirmMessage(['a'], PRUNE_DELETE_DATA_DEFAULT).includes('will be deleted too'),
    'the confirm must say the data goes, before anything is deleted',
  );
  assert.ok(
    pruneConfirmMessage(['a'], false).includes('left in place'),
    'the opt-out must be visible in the same sentence',
  );
});

// ── #6423: reviewing and settling an uncheckable registration ───────────────

test('an unjudged row is still kept out of every bulk selection', () => {
  // #6423 adds a per-row review, and this is the rule it must not weaken: the
  // batch reads `orphans` alone, so nothing an operator ticks can reach a row
  // the daemon declined to judge.
  const ids = selectableOrphans(census).map((r) => r.id);
  assert.ok(!ids.includes('ext-1'), 'a reviewable row is still never selectable');
  assert.deepEqual(
    unjudgedRows(census).map((r) => r.id),
    ['ext-1'],
    'the row is reviewable, which is a different list from the selectable one',
  );
});

test('the deregister confirmation names the path, not a count', () => {
  const message = unjudgedConfirmMessage(unjudgedRows(census)[0]);
  assert.ok(message.includes('/Volumes/Kemono/project'), message);
  assert.ok(message.includes('ext-1'), message);
  assert.ok(message.includes('cannot be undone'), message);
});

test('a colocated row is never told its data is already gone', () => {
  // Round-2 defect: the confirmation asserted "there is no index data to
  // delete" for every row. A colocated row keeps its data BESIDE the root, and
  // the daemon put the row in `indeterminate` because it could not tell whether
  // that root is gone — an unmounted volume's data is still there. The one
  // claim that holds either way is that nothing is deleted.
  const message = unjudgedConfirmMessage({ id: 'x', root_path: '/Volumes/K/p', colocated: true });
  assert.ok(message.includes('beside that root'), message);
  assert.ok(message.includes('may well still be there'), message);
  assert.ok(message.includes('left untouched'), message);
  assert.ok(!message.includes('no index data to delete'), message);
});

test('a non-colocated row is told where its data actually lives', () => {
  // The other half of the same defect: this data sits in trusty-search's own
  // directory, which is plainly on disk. It is not deleted either.
  const message = unjudgedConfirmMessage({ id: 'x', root_path: '/gone/x', colocated: false });
  assert.ok(message.includes("trusty-search's own directory"), message);
  assert.ok(message.includes('left untouched'), message);
  assert.ok(message.includes('only the registration is removed'), message);
});

test('a row with no colocated flag is read as not colocated', () => {
  const message = unjudgedConfirmMessage({ id: 'x', root_path: '/gone/x' });
  assert.ok(message.includes("trusty-search's own directory"), message);
});

test('the review note and the confirmation agree about the data', () => {
  // They are two renderings of one fact, and the round-2 defect was exactly a
  // note and a confirmation disagreeing about it.
  for (const colocated of [true, false]) {
    const row = { id: 'x', root_path: '/p', colocated };
    const fate = colocated ? 'may well still be there' : "trusty-search's own directory";
    assert.ok(unjudgedReviewNote(row).includes(fate), `note, colocated=${colocated}`);
    assert.ok(unjudgedConfirmMessage(row).includes(fate), `confirm, colocated=${colocated}`);
  }
});

test('the deregister confirmation offers no delete-data choice at all', () => {
  // The batch above defaults to deleting the data (#6422); this flow never
  // offers that, because the root could not be checked.
  for (const colocated of [true, false]) {
    const message = unjudgedConfirmMessage({ id: 'x', root_path: '/p', colocated });
    assert.ok(!/delete the data|deleted too/.test(message), message);
    assert.ok(message.includes('cannot be undone'), message);
  }
});

test('the confirmation survives a row missing its path rather than throwing', () => {
  const message = unjudgedConfirmMessage({ id: 'x' });
  assert.ok(message.includes('(unknown path)'), message);
});

test('a confirmed deregistration reads as done and says the data was left', () => {
  const out = readDeregisterResult(200, { ok: true, id: 'ext-1' });
  assert.equal(out.ok, true);
  assert.ok(out.message.includes('ext-1'), out.message);
  assert.ok(out.message.includes('left in place'), out.message);
});

test('a refused deregistration is a failure carrying the daemon message', () => {
  // Fail-closed: the console route refused, so nothing was removed. Reading
  // this as done leaves the operator believing a registration is gone.
  const out = readDeregisterResult(409, {
    ok: false,
    id: 'ext-1',
    error: "not deregistered: trusty-search no longer lists 'ext-1'",
  });
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('no longer lists'), out.message);
});

test('ok:false on a 200 deregistration still reads as failure', () => {
  const out = readDeregisterResult(200, { ok: false, id: 'ext-1', error: 'nothing was removed' });
  assert.equal(out.ok, false, 'the ok field decides, not the status code');
  assert.equal(out.message, 'nothing was removed');
});

test('an unparseable deregister answer is a failure, never a silent success', () => {
  const out = readDeregisterResult(502, null);
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('502'), out.message);
});
