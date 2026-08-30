/**
 * Tests for the palace/index delete flow helpers (#6360).
 *
 * Run: `node --test src/deleteFlow.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { KINDS, confirmMessage, deleteUrl, readDeleteResult } from './deleteFlow.js';

test('the confirm step names the exact id, for both kinds', () => {
  // The defect this prevents: a confirm that says "delete this palace?" leaves
  // an operator who clicked the wrong row with nothing to notice it by.
  for (const kind of Object.keys(KINDS)) {
    const msg = confirmMessage(kind, 'prod-corpus');
    assert.ok(msg.includes('prod-corpus'), `${kind} confirm must name the id: ${msg}`);
    assert.ok(msg.includes(KINDS[kind].noun), `${kind} confirm must name the kind: ${msg}`);
  }
});

test('two ids that differ produce two different confirm sentences', () => {
  assert.notEqual(confirmMessage('index', 'alpha'), confirmMessage('index', 'beta'));
});

test('#6422: an index delete starts with the on-disk data going, a palace does not', () => {
  // The owner ruling. Against the pre-fix code `KINDS.index` carried no
  // `optionDefault` at all and the checkbox started unticked, so a confirmed
  // delete deregistered and left the corpus — this assertion fails there.
  assert.equal(
    KINDS.index.optionDefault,
    true,
    'deleting an index must purge its on-disk data by default',
  );
  assert.equal(
    KINDS.palace.optionDefault,
    false,
    'force widens what a palace delete may destroy and stays opt-in',
  );
});

test('#6422: the index option is labelled as the keep-the-data opt-out', () => {
  // A ticked box whose label only says what ticking does leaves the operator
  // no way to read what unticking means.
  const label = KINDS.index.optionLabel;
  assert.ok(/untick/i.test(label), `the label must name the opt-out gesture: ${label}`);
  assert.ok(/deregister/i.test(label), `the label must name what unticking does: ${label}`);
});

test('the delete URL targets the owning route and encodes the id', () => {
  assert.equal(
    deleteUrl('palace', 'scratch', false),
    '/api/console/memory/palaces/scratch?force=false',
  );
  assert.equal(
    deleteUrl('index', 'scratch', true),
    '/api/console/search/indexes/scratch?delete_data=true',
  );
  assert.equal(
    deleteUrl('index', 'a/b', false),
    '/api/console/search/indexes/a%2Fb?delete_data=false',
    'a separator in an id must not become a path segment',
  );
});

test('an unknown kind throws rather than building a wrong URL', () => {
  assert.throws(() => deleteUrl('drawer', 'x', false), /unknown delete kind/);
});

test('a confirmed delete reads as success', () => {
  const out = readDeleteResult(200, { ok: true, id: 'scratch', detail: {} });
  assert.equal(out.ok, true);
  assert.ok(out.message.includes('scratch'));
});

test('a daemon refusal reads as failure carrying the daemon message', () => {
  const out = readDeleteResult(409, {
    ok: false,
    id: 'scratch',
    error: "trusty-memory refused palace_delete (code -32000): Palace 'scratch' still has 4 drawers",
  });
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('still has 4 drawers'), out.message);
});

test('a skipped no-op delete reads as failure, never as deleted', () => {
  // trusty-search answers 200 OK with removed:false for an index it never had.
  // The console route turns that into ok:false; this asserts the UI believes
  // that field rather than the status code.
  const out = readDeleteResult(409, {
    ok: false,
    id: 'ghost',
    error: "trusty-search skipped the delete: no registration for 'ghost' was removed",
    detail: { removed: false },
  });
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('skipped the delete'), out.message);
});

test('ok:false on a 200 still reads as failure', () => {
  const out = readDeleteResult(200, { ok: false, id: 'x', error: 'nothing was removed' });
  assert.equal(out.ok, false, 'the ok field decides, not the status code');
  assert.equal(out.message, 'nothing was removed');
});

test('an unparseable body still produces a failure with a usable message', () => {
  const out = readDeleteResult(503, null);
  assert.equal(out.ok, false);
  assert.ok(out.message.includes('503'), out.message);
});

test('a body missing ok is not treated as a success', () => {
  assert.equal(readDeleteResult(200, { id: 'x' }).ok, false);
  assert.equal(readDeleteResult(200, { ok: 'true' }).ok, false, 'a string is not true');
});
