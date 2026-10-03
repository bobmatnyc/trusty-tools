/*
 * Why: trusty-search holds an index whose restored exclude glob does not parse
 * (#9059). A held index indexes nothing new and refuses reindex and pushed
 * writes with `409 index_held`, while search keeps serving the old corpus. An
 * operator who sees only a stale index needs to learn it is held, which glob
 * caused it, and how to release it.
 * What: reads the hold from the two daemon fields that report it and returns
 * one shape the banner renders:
 *   - `GET /indexes/{id}/config` → `invalid_exclude_globs` (non-empty = held),
 *     defined on `IndexConfigView` in trusty-search
 *     `src/service/server/index_config.rs`;
 *   - `GET /indexes/{id}/status` → `status: "held"` with the daemon's reason in
 *     `last_walk_error`, set by `index_status_report` in
 *     `src/service/server/status.rs`.
 * Both return `null` for an index that is not held.
 * Test: `indexHold.test.js`.
 */

/** Reason shown when the daemon reports a hold without a reason string. */
const FALLBACK_REASON =
  'An exclude glob does not parse, so the paths it names cannot be excluded. ' +
  'Nothing new is indexed until it is fixed; search keeps serving what is already indexed.';

/**
 * @typedef {object} IndexHold
 * @property {string} indexId    The held index.
 * @property {string[]} patterns The exclude globs that do not parse ([] when unknown).
 * @property {string} reason     Why the index is held.
 */

/**
 * The hold a `GET /indexes/{id}/config` body reports, or `null`.
 *
 * @param {string} id               Index id the config was read for.
 * @param {object|null|undefined} config  The config body.
 * @returns {IndexHold|null}
 */
export function holdFromConfig(id, config) {
  const patterns = config?.invalid_exclude_globs;
  if (!Array.isArray(patterns) || patterns.length === 0) return null;
  return {
    indexId: id,
    patterns: [...patterns],
    reason:
      `${patterns.length === 1 ? 'This exclude glob does' : 'These exclude globs do'} not parse, ` +
      'so the paths they name cannot be excluded. Nothing new is indexed and reindex is ' +
      'refused until they are fixed; search keeps serving what is already indexed.'
  };
}

/**
 * The hold a `GET /indexes/{id}/status` body reports, or `null`.
 *
 * The status body names the globs only inside its reason string, so `patterns`
 * is empty here and the reason carries them.
 *
 * @param {string} id               Index id the status was read for.
 * @param {object|null|undefined} status  The status body.
 * @returns {IndexHold|null}
 */
export function holdFromStatus(id, status) {
  if (status?.status !== 'held') return null;
  const reason =
    typeof status.last_walk_error === 'string' && status.last_walk_error.trim() !== ''
      ? status.last_walk_error
      : FALLBACK_REASON;
  return { indexId: id, patterns: [], reason };
}
