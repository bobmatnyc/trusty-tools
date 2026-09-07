/**
 * Palace-compact helpers for the Memory tab (#6371).
 *
 * Why: compaction is a destructive-adjacent action whose confirm sentence and
 * answer-reading are easy to get wrong and impossible to see in a browser.
 * Keeping them here as pure functions is what makes them testable.
 *
 * #6941: this file also carried the stale-index prune and unjudged-deregister
 * decisions. The console is display-only (DOC-73 §13) and both of its routes are
 * removed, so those exports had no caller left; the same decisions now live in
 * the search dashboard at `crates/trusty-console/ui-search/src/lib/cleanup.js`,
 * which reads trusty-search's census directly.
 *
 * What: pure functions, no fetch and no DOM.
 * Test: `cleanupFlow.test.js` — run `node --test src/cleanupFlow.test.js` from
 * `crates/trusty-console/ui`.
 */

import { readActionResult } from './deleteFlow.js';

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
