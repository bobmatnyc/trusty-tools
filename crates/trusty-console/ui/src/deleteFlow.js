/**
 * Delete-flow helpers shared by the Memory and Search tabs (#6360).
 *
 * Why: both tabs delete a resource the same way — name the exact id in a
 * confirm step, call the console's DELETE route, then believe only what the
 * response says. Keeping the three decisions here (what the confirm says, what
 * URL to call, whether the answer was a success) makes them testable without a
 * browser, and keeps the two tabs from drifting into two different ideas of
 * what "deleted" means.
 *
 * What: three pure functions, no fetch and no DOM.
 * Test: `deleteFlow.test.js` — run `node --test src/deleteFlow.test.js` from
 * `crates/trusty-console/ui`.
 */

/**
 * The two resource kinds the console can delete, and where each one lives.
 *
 * `optionParam` is the query parameter the extra confirm checkbox toggles:
 * `force` lets trusty-memory tear down a palace that still holds drawers;
 * `delete_data` lets trusty-search destroy an index's on-disk corpus rather
 * than only deregistering it.
 *
 * `optionDefault` is where the checkbox starts, and the two kinds differ (#6422).
 * A palace delete starts unticked: `force` widens what a delete may destroy, so
 * a refusal on a non-empty palace is the safe answer. An index delete starts
 * TICKED, because the owner ruling made purging the on-disk data the default and
 * deregister-only the explicit opt-out — unticking the box is that opt-out.
 */
export const KINDS = {
  palace: {
    noun: 'palace',
    path: '/api/console/memory/palaces',
    optionParam: 'force',
    optionDefault: false,
    optionLabel: 'Delete even if it still holds drawers (force)',
  },
  index: {
    noun: 'index',
    path: '/api/console/search/indexes',
    optionParam: 'delete_data',
    optionDefault: true,
    optionLabel: 'Delete the on-disk data too — untick to deregister only and keep the corpus',
  },
};

/**
 * The sentence the confirm step shows before anything is deleted.
 *
 * Why: a destructive action must name the exact thing it is about to destroy,
 * so an operator who clicked the wrong row sees the wrong id here rather than
 * afterwards. The id is always present in the returned string — that is what
 * `deleteFlow.test.js` asserts.
 *
 * @param {string} kind One of the keys of {@link KINDS}.
 * @param {string} id The resource id.
 * @returns {string} The confirm sentence.
 */
export function confirmMessage(kind, id) {
  const noun = KINDS[kind]?.noun ?? 'resource';
  return `Delete ${noun} "${id}"? This cannot be undone.`;
}

/**
 * The console route that deletes `id`, with the extra option applied.
 *
 * Why: the id comes from daemon-reported data and is placed in a URL path, so
 * it is percent-encoded here rather than interpolated raw. The console's route
 * refuses anything outside `[A-Za-z0-9._-]` regardless; encoding means a
 * refusal reads as a refusal instead of as a request for a different path.
 *
 * @param {string} kind One of the keys of {@link KINDS}.
 * @param {string} id The resource id.
 * @param {boolean} option Whether the kind's extra option is enabled.
 * @returns {string} The request URL.
 */
export function deleteUrl(kind, id, option) {
  const spec = KINDS[kind];
  if (!spec) throw new Error(`unknown delete kind: ${kind}`);
  const flag = option === true ? 'true' : 'false';
  return `${spec.path}/${encodeURIComponent(id)}?${spec.optionParam}=${flag}`;
}

/**
 * Whether a delete actually happened, and what to tell the operator if not.
 *
 * Why (#6360): the daemons have failure modes that look like success on the
 * wire — trusty-search answers `200 OK` for an index it never had. The console
 * route already reduces that to an `ok` field, and this reads that field rather
 * than the status code, so a body that somehow arrives on a 2xx without a
 * confirmed delete still reads as a failure. The message shown is always the
 * daemon's own, never one invented here.
 *
 * @param {number} status HTTP status of the console's response.
 * @param {object|null} body Parsed JSON body, or null when it did not parse.
 * @returns {{ok: boolean, message: string}} Outcome and the text to display.
 */
export function readDeleteResult(status, body) {
  return readActionResult(status, body, 'delete', (b) => `Deleted "${b.id ?? ''}".`);
}

/**
 * The same reading, for any single-resource action the console routes.
 *
 * Why (#6371): compacting a palace has the identical contract — the console
 * route reduces the daemon's answer to an `ok` field, and the UI must believe
 * that field rather than the status code. A second copy of this reading is how
 * one action starts trusting a 2xx while the other does not.
 *
 * @param {number} status HTTP status of the console's response.
 * @param {object|null} body Parsed JSON body, or null when it did not parse.
 * @param {string} noun What failed, for the fallback message ("delete",
 *   "compaction").
 * @param {(body: object) => string} describeSuccess Builds the success message
 *   from the response body.
 * @returns {{ok: boolean, message: string}} Outcome and the text to display.
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
