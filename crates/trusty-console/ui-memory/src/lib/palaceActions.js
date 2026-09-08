/**
 * Compact and delete, moved here from the console's Memory tab (#6928).
 *
 * Why: the owner's ruling makes the console tab display-only and puts every
 * action on this dashboard. These are the same three decisions the tab used to
 * make — what the confirm step says, which route to call, and whether the
 * answer was a success — carried over unchanged so "deleted" and "compacted"
 * keep meaning what they meant. They were pure functions in
 * `ui/src/deleteFlow.js` and `ui/src/cleanupFlow.js`, which this replaces;
 * those files are gone, because nothing in the console panel deletes any more.
 *
 * What: pure functions, no fetch and no DOM. Paths are RELATIVE to the
 * dashboard's injected API base (`/api/memory/`), so `../console/…` resolves
 * to the console's own routes under any mount — including a proxy sub-path,
 * which an origin-absolute path would break.
 * Test: `palaceActions.test.js`.
 */

/**
 * The console route that compacts one palace, relative to the API base.
 *
 * The id is percent-encoded: the console route refuses anything outside
 * `[A-Za-z0-9._-]` regardless, and encoding means a refusal reads as a refusal
 * instead of as a request for a different path.
 *
 * @param {string} id palace id
 * @returns {string} path relative to `apiBase()`
 */
export function compactPath(id) {
  return `../console/memory/palaces/${encodeURIComponent(id)}/compact`;
}

/**
 * The console route that deletes one palace, relative to the API base.
 *
 * `force` starts unticked in the UI (#6422): it widens what a delete may
 * destroy, so a refusal on a palace that still holds drawers is the safe
 * answer.
 *
 * @param {string} id palace id
 * @param {boolean} force delete even when the palace still holds drawers
 * @returns {string} path relative to `apiBase()`
 */
export function deletePath(id, force) {
  return `../console/memory/palaces/${encodeURIComponent(id)}?force=${force === true}`;
}

/** What the delete confirm step says before anything is destroyed. */
export function deleteConfirmMessage(id) {
  return `Delete palace "${id}"? This cannot be undone.`;
}

/** The label on the force checkbox, so the operator reads what it widens. */
export const FORCE_LABEL = 'Delete even if it still holds drawers (force)';

/** What the compact confirm step says. */
export function compactConfirmMessage(id) {
  return `Compact palace "${id}"? This drops vector entries that have no drawer behind them.`;
}

/**
 * Whether a single-palace action happened, and what to tell the operator.
 *
 * Why it reads `ok` rather than the status code (#6360): the daemons have
 * failure modes that look like success on the wire, and the console route
 * already reduces the daemon's answer to that one field. The message shown on
 * a failure is always the daemon's own, never one invented here.
 *
 * @param {number} status HTTP status of the console's response
 * @param {object|null} body parsed JSON body, or null when it did not parse
 * @param {string} noun what failed, for the fallback message
 * @param {(body: object) => string} describeSuccess success message from the body
 * @returns {{ok: boolean, message: string}}
 */
export function readActionResult(status, body, noun, describeSuccess) {
  if (status === 200 && body && body.ok === true) {
    return { ok: true, message: describeSuccess(body) };
  }
  const reported = body && typeof body.error === 'string' ? body.error.trim() : '';
  return {
    ok: false,
    message: reported || `The ${noun} failed (HTTP ${status}) and the daemon gave no reason.`,
  };
}

/** Read a delete answer. */
export function readDeleteResult(status, body) {
  return readActionResult(status, body, 'delete', (b) => `Deleted "${b.id ?? ''}".`);
}

/**
 * Read a compact answer, reporting the counts the daemon returned so
 * "compacted" is a claim with a number attached.
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
