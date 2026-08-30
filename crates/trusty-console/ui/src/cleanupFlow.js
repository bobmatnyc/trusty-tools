/**
 * Cleanup-flow helpers for the Search and Memory tabs (#6371).
 *
 * Why: the stale-registration cleanup has three decisions that are easy to get
 * wrong and impossible to see in a browser — which rows may be offered for
 * deletion, what the confirm step says before anything is deleted, and how to
 * read a batch answer where some ids succeeded and some did not. Keeping them
 * here as pure functions is what makes them testable, and keeps the tab from
 * inventing its own idea of "stale".
 *
 * The console decides NOTHING about staleness. The candidate list is the
 * trusty-search daemon's own census (`GET /registry/orphans`, reached through
 * the console's reverse proxy), which splits gone roots from roots it could not
 * judge. `selectableOrphans` reads the `orphans` list and never the
 * `indeterminate` one — a root that could not be checked is not a stale root.
 *
 * What: pure functions, no fetch and no DOM.
 * Test: `cleanupFlow.test.js` — run `node --test src/cleanupFlow.test.js` from
 * `crates/trusty-console/ui`.
 */

import { readActionResult } from './deleteFlow.js';

/** Where the daemon's own orphan census is reached, through the console proxy. */
export const CENSUS_URL = '/api/search/registry/orphans';

/** The console route that deletes a confirmed batch of registrations. */
export const PRUNE_URL = '/api/console/search/prune-indexes';

/**
 * Where the prune panel's "delete the on-disk data too" checkbox starts.
 *
 * `true` since #6422: the owner ruling made purging the data the default on
 * every delete-index surface, and keeping it the explicit opt-out. A stale
 * registration's corpus is the disk this whole panel exists to reclaim, so
 * unticking the box is the exception. Exported rather than written inline in
 * `StaleIndexCleanup.svelte` so the default is covered by `cleanupFlow.test.js`.
 */
export const PRUNE_DELETE_DATA_DEFAULT = true;

/**
 * The registrations this UI may offer for deletion.
 *
 * Why (#6371): the census reports two lists for a reason. `orphans` is what the
 * daemon is willing to call gone; `indeterminate` is every root it declined to
 * judge — an unmounted volume, a root whose parent is also missing. Offering
 * the second for deletion is how an operator deletes a volume's whole index
 * roster the moment it is unplugged. Unknown is not stale, so this function
 * reads one list and ignores the other.
 *
 * @param {object|null} census The parsed census body.
 * @returns {{id: string, root_path: string}[]} Deletion candidates, in census
 *   order. Empty for a malformed or absent census.
 */
export function selectableOrphans(census) {
  const rows = census && Array.isArray(census.orphans) ? census.orphans : [];
  return rows.filter((row) => row && typeof row.id === 'string' && row.id.length > 0);
}

/**
 * The rows the census reported but declined to judge.
 *
 * These are shown, never selected — an operator who sees a root listed as
 * unjudged knows the daemon looked at it, which is different from the daemon
 * never having seen it.
 *
 * @param {object|null} census The parsed census body.
 * @returns {{id: string, root_path: string, reason: string}[]} The unjudged rows.
 */
export function unjudgedRows(census) {
  return census && Array.isArray(census.indeterminate) ? census.indeterminate : [];
}

/**
 * The one-line summary of a census.
 *
 * @param {object|null} census The parsed census body.
 * @returns {string} A sentence naming the counts.
 */
export function censusSummary(census) {
  const stale = selectableOrphans(census).length;
  const unjudged = unjudgedRows(census).length;
  const total = census && typeof census.total === 'number' ? census.total : 0;
  if (stale === 0 && unjudged === 0) {
    return `No stale registrations. ${total} registered.`;
  }
  const parts = [`${stale} stale of ${total} registered`];
  if (unjudged > 0) parts.push(`${unjudged} could not be checked and will not be removed`);
  return `${parts.join('; ')}.`;
}

/**
 * The sentence the confirm step shows before anything is deleted.
 *
 * Why: a destructive batch must say how many things it is about to destroy and
 * whether it will take their data with it. The ids themselves are listed beside
 * this sentence rather than crammed into it — the count is what the operator
 * checks, the list is what they scan.
 *
 * @param {string[]} ids The ids about to be deleted.
 * @param {boolean} deleteData Whether the on-disk corpus goes too.
 * @returns {string} The confirm sentence.
 */
export function pruneConfirmMessage(ids, deleteData) {
  const n = ids.length;
  const noun = n === 1 ? 'registration' : 'registrations';
  const fate = deleteData
    ? 'Their on-disk index data will be deleted too.'
    : 'Their on-disk index data will be left in place.';
  return `Remove ${n} stale ${noun}? ${fate} This cannot be undone.`;
}

/**
 * Whether a prune batch fully succeeded, and what to say about it.
 *
 * Why (#6371): a batch has no single outcome, and reporting one is the failure
 * this whole route exists to avoid — three ids removed and one refused is not
 * "cleaned". The console route answers `ok` plus a row per id; this reads both,
 * and the rows it returns are the daemon's own, never re-derived.
 *
 * @param {number} status HTTP status of the console's response.
 * @param {object|null} body Parsed JSON body, or null when it did not parse.
 * @returns {{ok: boolean, message: string, rows: object[]}} Outcome, the
 *   sentence to display, and one row per requested id.
 */
export function readPruneResult(status, body) {
  const rows = body && Array.isArray(body.results) ? body.results : [];
  if (body && body.ok === true && rows.length > 0) {
    const n = body.removed ?? rows.length;
    return {
      ok: true,
      message: `Removed ${n} stale registration${n === 1 ? '' : 's'}.`,
      rows,
    };
  }
  const removed = body && typeof body.removed === 'number' ? body.removed : 0;
  const failed = body && typeof body.failed === 'number' ? body.failed : 0;
  if (rows.length > 0) {
    return {
      ok: false,
      message: `Removed ${removed}; ${failed} could not be removed. Each is listed below with the daemon's reason.`,
      rows,
    };
  }
  const reported = body && typeof body.error === 'string' ? body.error.trim() : '';
  return {
    ok: false,
    message: reported || `The prune failed (HTTP ${status}) and the daemon gave no reason.`,
    rows,
  };
}

/**
 * The console route that compacts one palace.
 *
 * The id is percent-encoded for the same reason the delete URLs encode theirs:
 * the console route refuses anything outside `[A-Za-z0-9._-]` regardless, and
 * encoding means a refusal reads as a refusal instead of as a request for a
 * different path.
 *
 * @param {string} id The palace id.
 * @returns {string} The request URL.
 */
export function compactUrl(id) {
  return `/api/console/memory/palaces/${encodeURIComponent(id)}/compact`;
}

/**
 * The sentence the compact confirm step shows.
 *
 * @param {string} id The palace id.
 * @returns {string} The confirm sentence.
 */
export function compactConfirmMessage(id) {
  return `Compact palace "${id}"? This drops vector entries that have no drawer behind them.`;
}

/**
 * Whether a compaction happened, and what to tell the operator.
 *
 * Reads the `ok` field rather than the status code, exactly as the delete flow
 * does, and reports the counts the daemon returned so "compacted" is a claim
 * with a number attached.
 *
 * @param {number} status HTTP status of the console's response.
 * @param {object|null} body Parsed JSON body, or null when it did not parse.
 * @returns {{ok: boolean, message: string}} Outcome and the text to display.
 */
export function readCompactResult(status, body) {
  return readActionResult(status, body, 'compaction', (b) => {
    const removed = b.detail?.orphans_removed;
    const checked = b.detail?.total_checked;
    if (typeof removed === 'number' && typeof checked === 'number') {
      return `Compacted "${b.id ?? ''}": reclaimed ${removed} of ${checked} vector entries.`;
    }
    return `Compacted "${b.id ?? ''}".`;
  });
}
