/**
 * Tests for the palace-compact helpers (#6371).
 *
 * #6941 removed the prune / deregister cases with the exports they covered; the
 * ported decisions are tested in `ui-search/src/lib/cleanup.test.js`.
 *
 * Run: `node --test src/cleanupFlow.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { compactConfirmMessage, compactUrl, readCompactResult } from './cleanupFlow.js';

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
