/**
 * How the Sessions tab buckets, dates, and orders one session row
 * (#6430 last-used sort, #6431 unknown-bucket selection).
 *
 * Why: both features turn on the same two decisions the tab used to make inline
 * — which bucket a record lands in, and what its last activity is — and neither
 * was testable without a browser. #6431 in particular needs the unknown set to
 * be a named, tested predicate rather than a `||` fallback in a template: it is
 * the target set of a destructive bulk action, so "which sessions does this
 * delete" has to be answerable from a test.
 *
 * What: pure functions over the records `GET /api/console/sessions` returns. No
 * fetch, no DOM. `session_list` merges two registries, so a row is either a
 * managed record (`state`, `last_activity_at` as RFC 3339) or a legacy registry
 * entry (`status`, `last_seen` as serde's `SystemTime` object) — these functions
 * read both. The last-used FORMATTING and comparator come from `./lastUsed.js`
 * (#6424), so all three tabs read one column the same way; this module adds only
 * the two-wire-shape normalisation the other tabs do not need.
 * Test: `sessionRows.test.js` — run `node --test src/sessionRows.test.js` from
 * `crates/trusty-console/ui`.
 */

import {
  formatLastUsed,
  lastUsedTitle,
  sortByLastUsed as sortRowsByLastUsed,
} from './lastUsed.js';

export { NEVER } from './lastUsed.js';

/**
 * Lifecycle states the tab gives their own group, in display order.
 *
 * `deleted` is here as of #6431. It is a real `ManagedSessionState` variant, and
 * leaving it out put every soft-deleted tombstone in the catch-all bucket
 * alongside the genuinely-unmodelled records — which made the bulk action's
 * target set mean two different things at once. Tombstones stay LISTED (hiding
 * a record is how a fleet loses track of one); they just have their own group.
 */
export const STATE_ORDER = [
  'active',
  'provisioning',
  'stopped',
  'errored',
  'decommissioned',
  'deleted',
];

/** Catch-all bucket for a record whose state is missing or unrecognised. */
export const OTHER_STATE = 'other';

/** Display order: the known states, then the catch-all. */
export const GROUP_ORDER = [...STATE_ORDER, OTHER_STATE];

/**
 * The lifecycle label to show on a row.
 *
 * A managed record carries `state`. A legacy registry entry carries no `state`
 * at all — that absence is the entire reason the "unknown" bucket exists — so
 * it reads as `unknown` rather than borrowing its `status`, which is a different
 * enum with different meanings.
 */
export function rawState(session) {
  const state = session?.state;
  return typeof state === 'string' && state.length > 0
    ? state.toLowerCase()
    : 'unknown';
}

/** The group a row belongs to: its own state, or the catch-all. */
export function bucketOf(session) {
  const st = rawState(session);
  return STATE_ORDER.includes(st) ? st : OTHER_STATE;
}

/**
 * Whether this row is in the unknown bucket — the ONLY set the bulk delete
 * targets.
 *
 * This is deliberately `bucketOf(...) === OTHER_STATE` rather than a separate
 * rule: the button deletes what the "other" heading shows, and one predicate is
 * what keeps those two from drifting.
 */
export function isUnknown(session) {
  return bucketOf(session) === OTHER_STATE;
}

/** Group every session by bucket, with every bucket key always present. */
export function groupByState(sessions) {
  const grouped = {};
  for (const key of GROUP_ORDER) grouped[key] = [];
  for (const session of sessions ?? []) grouped[bucketOf(session)].push(session);
  return grouped;
}

/**
 * A session's last activity in UNIX SECONDS, or `null` when it has none.
 *
 * This is the whole of what the Sessions tab adds to `lastUsed.js`: search
 * indexes and memory palaces send one `last_used_unix` field, and `session_list`
 * sends two other shapes because it merges two registries — a managed record's
 * `last_activity_at` (RFC 3339, `null` until the session reports) and a legacy
 * entry's `last_seen` (serde's `SystemTime`, an object of `secs_since_epoch` /
 * `nanos_since_epoch`). Normalising here means the formatting, the never-used
 * rule, and the comparator stay in the one module every tab shares.
 *
 * `created_at` is NOT a fallback: a creation date under a "last used" heading is
 * a wrong answer, and "never" is the honest one.
 */
export function lastUsedUnix(session) {
  const direct = session?.last_used_unix;
  if (typeof direct === 'number' && Number.isFinite(direct) && direct > 0) {
    return direct;
  }
  const activity = session?.last_activity_at;
  if (typeof activity === 'string') {
    const ms = Date.parse(activity);
    if (!Number.isNaN(ms)) return Math.floor(ms / 1000);
  }
  const seen = session?.last_seen;
  if (seen && typeof seen.secs_since_epoch === 'number') {
    return seen.secs_since_epoch;
  }
  if (typeof seen === 'string') {
    const ms = Date.parse(seen);
    if (!Number.isNaN(ms)) return Math.floor(ms / 1000);
  }
  return null;
}

/** The shape `lastUsed.js` reads, for one session record. */
function asLastUsedRow(session) {
  return { last_used_unix: lastUsedUnix(session) };
}

/**
 * The text for a session's last-used cell — the same relative-then-absolute
 * wording the Search and Memory tabs use, so one column means one thing across
 * the console. `now` is unix seconds, injected so tests are deterministic.
 */
export function lastUsedLabel(session, now) {
  return now === undefined
    ? formatLastUsed(asLastUsedRow(session))
    : formatLastUsed(asLastUsedRow(session), now);
}

/** Full timestamp for the cell's `title`, or the never-used explanation. */
export function lastUsedTitleFor(session) {
  return lastUsedTitle(asLastUsedRow(session));
}

/**
 * Order sessions by last activity, newest first for `'desc'`.
 *
 * Delegates to `lastUsed.js`'s comparator so the never-used-sorts-last rule and
 * the stable tie order are defined once for every tab. Only the timestamp
 * extraction differs, and that is [`lastUsedUnix`].
 */
export function sortByLastUsed(sessions, direction = 'desc') {
  return sortRowsByLastUsed(
    (sessions ?? []).map((session) => ({
      session,
      last_used_unix: lastUsedUnix(session),
    })),
    direction,
  ).map((row) => row.session);
}

/** The label a row shows for itself, and the tie-breaker for sorting. */
export function nameOf(session) {
  return session?.name || session?.tmux_name || session?.id || '';
}

/**
 * A legacy entry's reported `status`, for the confirmation dialog.
 *
 * Why: an unknown-bucket row shows `unknown` as its lifecycle label, because it
 * has no `state`. It usually does carry a `status` — `Active`, `Stopped` — and
 * showing that before a destructive confirmation is the difference between an
 * operator seeing what they are about to delete and not. The daemon refuses a
 * running session regardless (that guard is not this label's job); this only
 * stops the dialog from hiding what it knows.
 * Returns `null` for a record with no `status`, so the row simply omits it.
 */
export function reportedStatus(session) {
  const status = session?.status;
  return typeof status === 'string' && status.length > 0 ? status : null;
}

/**
 * Summarise a `session_delete_records` response for the action line.
 *
 * Fail-closed: a response the UI cannot read reports zero deletions, never an
 * optimistic success. A partial run says so explicitly — "3 of 5" — because a
 * bare "deleted" on a run that half-failed is the message that loses records.
 */
export function summariseBulkDelete(payload) {
  const deleted = Number.isInteger(payload?.deleted) ? payload.deleted : 0;
  const failed = Number.isInteger(payload?.failed) ? payload.failed : 0;
  const requested = Number.isInteger(payload?.requested)
    ? payload.requested
    : deleted + failed;
  if (requested === 0) return 'nothing to delete';
  if (failed === 0) return `deleted ${deleted} of ${requested} records`;
  if (deleted === 0) return `deleted none of ${requested} records — ${failed} failed`;
  return `partial: deleted ${deleted} of ${requested} records — ${failed} failed`;
}

/** The per-session failure rows, for the operator to read after a partial run. */
export function failedRows(payload) {
  const rows = Array.isArray(payload?.results) ? payload.results : [];
  return rows.filter((row) => row?.deleted !== true);
}
