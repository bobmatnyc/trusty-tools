/**
 * Tests for the Sessions tab's row bucketing, last-used sort, and bulk-delete
 * reporting (#6430, #6431).
 *
 * Run: `node --test src/sessionRows.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  GROUP_ORDER,
  NEVER,
  OTHER_STATE,
  STATE_ORDER,
  bucketOf,
  failedRows,
  groupByState,
  isUnknown,
  lastUsedLabel,
  lastUsedTitleFor,
  lastUsedUnix,
  nameOf,
  rawState,
  reportedStatus,
  sortByLastUsed,
  summariseBulkDelete,
} from './sessionRows.js';

/** A managed record, as `record_to_json` emits it. */
const managed = (over = {}) => ({
  kind: 'managed',
  id: '41c0cd97-d7d5-401b-8ef3-d07ef73e028a',
  name: 'tm-oracle',
  state: 'active',
  last_activity_at: '2026-08-27T23:12:21.458135+00:00',
  ...over,
});

/** A legacy registry entry: `status`, no `state`, `last_seen` as SystemTime. */
const legacy = (over = {}) => ({
  kind: 'legacy',
  id: 'c45486e4-46ae-402f-9f64-43489bae4876',
  tmux_name: 'tm-apex-companion-02',
  status: 'Active',
  last_seen: { secs_since_epoch: 1788021931, nanos_since_epoch: 192827000 },
  ...over,
});

test('a legacy entry has no state and lands in the unknown bucket', () => {
  // This is the whole of #6431's target set: `session_list` merges two
  // registries, and only the managed one carries `state`.
  const row = legacy();
  assert.equal(rawState(row), 'unknown');
  assert.equal(bucketOf(row), OTHER_STATE);
  assert.equal(isUnknown(row), true);
});

test('a deleted tombstone is its own group, not the unknown bucket', () => {
  // The reported defect: `deleted` is a real ManagedSessionState variant that
  // was absent from STATE_ORDER, so 17 tombstones sat in "other" alongside the
  // legacy entries and would have been swept up by a bulk action aimed at them.
  const row = managed({ state: 'deleted' });
  assert.equal(bucketOf(row), 'deleted');
  assert.equal(isUnknown(row), false);
  assert.ok(STATE_ORDER.includes('deleted'));
});

test('every modelled lifecycle state stays out of the unknown bucket', () => {
  for (const state of [
    'active',
    'provisioning',
    'stopped',
    'errored',
    'decommissioned',
    'deleted',
  ]) {
    assert.equal(isUnknown(managed({ state })), false, state);
  }
});

test('an unrecognised state string is unknown, and so is an empty one', () => {
  assert.equal(isUnknown(managed({ state: 'hibernating' })), true);
  assert.equal(isUnknown(managed({ state: '' })), true);
  assert.equal(isUnknown(managed({ state: null })), true);
});

test('state matching is case-insensitive', () => {
  assert.equal(bucketOf(managed({ state: 'ACTIVE' })), 'active');
});

test('grouping keeps every bucket present and drops nothing', () => {
  const rows = [
    managed({ id: 'a', state: 'active' }),
    managed({ id: 'b', state: 'deleted' }),
    legacy({ id: 'c' }),
    managed({ id: 'd', state: 'hibernating' }),
  ];
  const grouped = groupByState(rows);
  assert.deepEqual(Object.keys(grouped), GROUP_ORDER);
  assert.equal(grouped.active.length, 1);
  assert.equal(grouped.deleted.length, 1);
  assert.equal(grouped[OTHER_STATE].length, 2);
  const total = GROUP_ORDER.reduce((n, key) => n + grouped[key].length, 0);
  assert.equal(total, rows.length, 'no row may be silently dropped');
});

test('last-used reads every wire shape and refuses to invent one', () => {
  // The Sessions tab is the only surface that sees three shapes: #6424's
  // `last_used_unix`, a managed record's RFC 3339 stamp, and a legacy entry's
  // serde SystemTime object.
  assert.equal(lastUsedUnix({ last_used_unix: 1788021931 }), 1788021931);
  assert.equal(
    lastUsedUnix(managed({ last_activity_at: '2026-08-27T23:12:21Z' })),
    Math.floor(Date.parse('2026-08-27T23:12:21Z') / 1000),
  );
  assert.equal(lastUsedUnix(legacy()), 1788021931);
  // A managed record that has never reported activity: null, not created_at.
  assert.equal(
    lastUsedUnix(managed({ last_activity_at: null, created_at: '2026-08-01T00:00:00Z' })),
    null,
  );
  assert.equal(lastUsedUnix(managed({ last_activity_at: 'not a date' })), null);
  assert.equal(lastUsedUnix({ last_used_unix: 0 }), null, 'the epoch is not an answer');
});

test('a row with no activity renders as the never cell, with an explanation', () => {
  assert.equal(lastUsedLabel(managed({ last_activity_at: null })), NEVER);
  assert.match(lastUsedTitleFor(managed({ last_activity_at: null })), /Never used/);
  assert.notEqual(lastUsedLabel(managed()), NEVER);
});

test('a session cell uses the same wording as the search and memory tabs', () => {
  // Shared formatting from #6424's lastUsed.js, not a second implementation —
  // one "Last used" column must not mean two different things in one console.
  const at = Math.floor(Date.parse('2026-08-27T23:12:21Z') / 1000);
  assert.equal(lastUsedLabel(managed({ last_activity_at: '2026-08-27T23:12:21Z' }), at), 'just now');
  assert.equal(lastUsedLabel(managed({ last_activity_at: '2026-08-27T21:12:21Z' }), at), '2h ago');
  assert.equal(lastUsedLabel(legacy(), 1788021931 + 3 * 24 * 3600), '3d ago');
});

test('desc puts the most recent first and nulls last', () => {
  const rows = [
    managed({ id: 'old', name: 'old', last_activity_at: '2026-08-01T00:00:00Z' }),
    managed({ id: 'none', name: 'none', last_activity_at: null }),
    managed({ id: 'new', name: 'new', last_activity_at: '2026-08-29T00:00:00Z' }),
  ];
  assert.deepEqual(
    sortByLastUsed(rows, 'desc').map((r) => r.id),
    ['new', 'old', 'none'],
  );
});

test('asc reverses the dated rows but still sorts nulls last', () => {
  // The trap this pins: treating null as 0 would put every never-used session
  // first under asc and bury the real answers.
  const rows = [
    managed({ id: 'new', name: 'new', last_activity_at: '2026-08-29T00:00:00Z' }),
    managed({ id: 'none', name: 'none', last_activity_at: null }),
    managed({ id: 'old', name: 'old', last_activity_at: '2026-08-01T00:00:00Z' }),
  ];
  assert.deepEqual(
    sortByLastUsed(rows, 'asc').map((r) => r.id),
    ['old', 'new', 'none'],
  );
});

test('sorting does not mutate the input and is stable on ties', () => {
  const rows = [
    managed({ id: 'b', name: 'b', last_activity_at: '2026-08-01T00:00:00Z' }),
    managed({ id: 'a', name: 'a', last_activity_at: '2026-08-01T00:00:00Z' }),
  ];
  const sorted = sortByLastUsed(rows, 'desc');
  // Ties keep the order the daemon sent, so a poll does not reshuffle the list.
  assert.deepEqual(sorted.map((r) => r.id), ['b', 'a']);
  assert.deepEqual(rows.map((r) => r.id), ['b', 'a'], 'input must not be reordered');
});

test('a legacy row sorts by last_seen alongside managed rows', () => {
  const rows = [
    managed({ id: 'm', name: 'm', last_activity_at: '2026-01-01T00:00:00Z' }),
    legacy({ id: 'l', tmux_name: 'l' }),
  ];
  assert.equal(sortByLastUsed(rows, 'desc')[0].id, 'l');
});

test('the confirmation can show a legacy row its reported status', () => {
  // An unknown-bucket row labels itself `unknown`, so the dialog would
  // otherwise hide the one liveness hint the payload carries.
  assert.equal(reportedStatus(legacy()), 'Active');
  assert.equal(reportedStatus(legacy({ status: 'Stopped' })), 'Stopped');
  assert.equal(reportedStatus(managed()), null, 'a managed row has no status field');
  assert.equal(reportedStatus({ status: '' }), null);
});

test('a name falls back through name, tmux_name, then id', () => {
  assert.equal(nameOf({ name: 'n', tmux_name: 't', id: 'i' }), 'n');
  assert.equal(nameOf({ tmux_name: 't', id: 'i' }), 't');
  assert.equal(nameOf({ id: 'i' }), 'i');
});

test('a fully successful bulk delete reports the count', () => {
  assert.equal(
    summariseBulkDelete({ requested: 3, deleted: 3, failed: 0, results: [] }),
    'deleted 3 of 3 records',
  );
});

test('a partial bulk delete says partial and never rounds up', () => {
  // Fail-closed reporting: the operator must not read "deleted" for a run where
  // two records are still there.
  assert.equal(
    summariseBulkDelete({ requested: 5, deleted: 3, failed: 2, results: [] }),
    'partial: deleted 3 of 5 records — 2 failed',
  );
  assert.equal(
    summariseBulkDelete({ requested: 2, deleted: 0, failed: 2, results: [] }),
    'deleted none of 2 records — 2 failed',
  );
});

test('an unreadable response reports zero deletions, not success', () => {
  assert.equal(summariseBulkDelete(null), 'nothing to delete');
  assert.equal(summariseBulkDelete({}), 'nothing to delete');
  assert.equal(summariseBulkDelete({ deleted: 'many' }), 'nothing to delete');
});

test('failed rows are exactly the rows that did not report deleted:true', () => {
  const payload = {
    requested: 3,
    deleted: 1,
    failed: 2,
    results: [
      { session_id: 'a', deleted: true, error: null },
      { session_id: 'b', deleted: false, error: 'session is active' },
      { session_id: 'c', error: 'no session record found' },
    ],
  };
  assert.deepEqual(failedRows(payload).map((r) => r.session_id), ['b', 'c']);
  assert.deepEqual(failedRows(null), []);
});
