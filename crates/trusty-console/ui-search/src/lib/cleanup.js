/**
 * Stale-registration cleanup decisions (#6941).
 *
 * Why: `indexes.toml` accumulates registrations whose root was wiped, and 60 of
 * them on the owner's machine kept `warm_boot_degraded` true with no UI path to
 * clear them (#6371). The console carried that panel until #6923 made the
 * console display-only; DOC-73 §13 puts every control on the dashboard, so the
 * panel lives here now and talks to trusty-search's own API instead of a console
 * proxy route.
 *
 * The dashboard decides NOTHING about staleness. The candidate list is the
 * daemon's own census (`GET /registry/orphans`), which splits gone roots from
 * roots it declined to judge. Two rules from prior incidents are enforced here
 * rather than in prose:
 *
 * 1. A destructive operation is gated on the census's ROOT classification and
 *    on `chunk_count`, never on `size_bytes` / `disk_bytes`. Those metrics read
 *    `0` for a healthy 71,433-chunk colocated index (#4706), and an operator
 *    already diagnosed eleven live indexes as broken from that reading.
 *    [`destructiveSignals`] is the whole input to the gate, and it carries no
 *    size key.
 * 2. `DELETE /indexes/{id}` answers `removed: false` when it removed nothing —
 *    an id in no store and no registry answers `404 removed:false`, and a delete
 *    whose durable cleanup failed answers `500 ok:false` (#6363). A caller that
 *    reads the status code alone records removals that did not happen, which is
 *    the leak #6371 exists for. [`readDeleteOutcome`] believes the BODY.
 *
 * What: pure functions. No fetch, no DOM.
 * Test: `cleanup.test.js` — `pnpm test` from `crates/trusty-console/ui-search`.
 */

/** The daemon's own orphan census. Read-only; it removes nothing. */
export const CENSUS_PATH = '/registry/orphans';

/**
 * Where the prune panel's "delete the on-disk data too" checkbox starts.
 *
 * `true` since #6422: the owner ruling made purging the data the default on
 * every delete-index surface, and keeping it the explicit opt-out. A stale
 * registration's corpus is the disk this panel exists to reclaim.
 */
export const PRUNE_DELETE_DATA_DEFAULT = true;

/**
 * The registrations this UI may offer for the batch prune.
 *
 * Why (#6371): the census reports two lists for a reason. `orphans` is what the
 * daemon is willing to call gone; `indeterminate` is every root it declined to
 * judge — an unmounted volume, a root whose parent is also missing. Offering the
 * second for deletion is how an operator deletes a volume's whole index roster
 * the moment it is unplugged. Unknown is not stale, so this reads one list and
 * ignores the other.
 *
 * What: census order, each row tagged `rootState: 'orphaned'` so the gate below
 * reads a classification rather than re-deriving one.
 *
 * @param {object|null} census The parsed census body.
 * @returns {{id: string, root_path: string, rootState: 'orphaned'}[]}
 */
export function selectableOrphans(census) {
  const rows = census && Array.isArray(census.orphans) ? census.orphans : [];
  return rows
    .filter((row) => row && typeof row.id === 'string' && row.id.length > 0)
    .map((row) => ({ ...row, rootState: 'orphaned' }));
}

/**
 * The rows the census reported but declined to judge.
 *
 * Shown, never selected: an operator who sees a root listed as unjudged knows
 * the daemon looked at it, which is different from the daemon never having seen
 * it. Each is settled on its own through the per-row review, and `selected` in
 * the panel is built from [`selectableOrphans`] alone, so no bulk action can
 * sweep one in.
 *
 * @param {object|null} census The parsed census body.
 * @returns {{id: string, root_path: string, reason: string, colocated?: boolean,
 *   repo_identity?: string|null, rootState: 'indeterminate'}[]}
 */
export function unjudgedRows(census) {
  const rows = census && Array.isArray(census.indeterminate) ? census.indeterminate : [];
  return rows
    .filter((row) => row && typeof row.id === 'string' && row.id.length > 0)
    .map((row) => ({ ...row, rootState: 'indeterminate' }));
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
  if (unjudged > 0) parts.push(`${unjudged} could not be checked and are not in the batch`);
  return `${parts.join('; ')}.`;
}

/**
 * The chunk count one status body reports, or `null` when it did not say.
 *
 * Why `null` rather than `0`: a registration the warm-boot allowlist excluded is
 * absent from the live registry, so `GET /indexes/{id}/status` answers `404` and
 * this panel has NO count for it. Rendering that as `0` claims the index is
 * empty, which is a claim about content the daemon never made — and the
 * difference decides whether the confirm step warns about losing chunks.
 *
 * @param {object|null} status Parsed `GET /indexes/{id}/status` body.
 * @returns {number|null}
 */
export function chunkCountOf(status) {
  const n = status && status.chunk_count;
  return typeof n === 'number' && Number.isFinite(n) ? n : null;
}

/**
 * Every signal the destructive gate is allowed to read.
 *
 * Why this exists as its own function (#4706): the gate's inputs are the claim
 * worth testing. `size_bytes` (the roster's field) and `disk_bytes` (the status
 * body's, and the roster row's local name for it) both read `0` for a healthy
 * colocated index because they measured only the legacy global directory. An
 * operator read `0` against eleven live indexes and considered deleting them.
 * Neither key appears in what this returns, so a gate written in terms of it
 * cannot consult one.
 *
 * What: the census's root classification and the chunk count, and nothing else.
 * Test: `destructive_signals_carry_no_byte_metric`,
 * `eligibility_is_unchanged_by_any_size_metric`.
 *
 * @param {{rootState?: string}} row One census row.
 * @param {object|null} status That id's status body, or null.
 * @returns {{rootState: string, chunkCount: number|null}}
 */
export function destructiveSignals(row, status) {
  return {
    rootState: row && typeof row.rootState === 'string' ? row.rootState : 'unknown',
    chunkCount: chunkCountOf(status)
  };
}

/**
 * Whether one census row may be swept by the batch prune, and why.
 *
 * Why: this is the only place the panel decides that something may be destroyed,
 * and it decides it from [`destructiveSignals`] alone — the daemon's root
 * classification plus a chunk count, never a byte total (#4706).
 * What: `orphaned` — the daemon says the root is gone — is the one eligible
 * state. Everything else is refused with the reason shown beside the row.
 * `chunkCount` rides along so the confirm step can say what is at stake without
 * a second lookup.
 * Test: `only_a_gone_root_is_eligible`,
 * `an_unjudged_row_is_never_eligible_however_large`,
 * `eligibility_is_unchanged_by_any_size_metric`.
 *
 * @param {{rootState?: string}} row One census row.
 * @param {object|null} status That id's status body, or null.
 * @returns {{eligible: boolean, reason: string, chunkCount: number|null}}
 */
export function pruneEligibility(row, status) {
  const { rootState, chunkCount } = destructiveSignals(row, status);
  if (rootState === 'orphaned') {
    return {
      eligible: true,
      reason: 'trusty-search reports this root is gone from disk.',
      chunkCount
    };
  }
  if (rootState === 'indeterminate') {
    return {
      eligible: false,
      reason:
        'trusty-search could not check this root, so it is not in the batch. ' +
        'Review it on its own to settle it.',
      chunkCount
    };
  }
  return {
    eligible: false,
    reason: 'trusty-search did not classify this root, so nothing here may delete it.',
    chunkCount
  };
}

/**
 * What one candidate stands to lose, in chunks.
 *
 * Why chunks and not bytes: see [`destructiveSignals`]. An unknown count says so
 * rather than reading as an empty index.
 *
 * @param {number|null} chunkCount The count from [`chunkCountOf`].
 * @returns {string} A clause for the row and the confirm list.
 */
export function impactPhrase(chunkCount) {
  if (chunkCount === null) {
    return 'chunk count unknown — the daemon has not loaded this registration';
  }
  if (chunkCount === 0) return 'no indexed chunks';
  return `${chunkCount.toLocaleString('en-US')} indexed chunk${chunkCount === 1 ? '' : 's'}`;
}

/**
 * The sentence the confirm step shows before anything is deleted.
 *
 * Why: a destructive batch must say how many things it is about to destroy and
 * whether it will take their data with it. The ids themselves are listed beside
 * this sentence rather than crammed into it — the count is what the operator
 * checks, the list is what they scan.
 *
 * What: names the batch size, the total chunk count where every candidate's
 * status was readable, and the fate of the on-disk corpus. A batch holding an
 * unknown count says so instead of understating the total.
 * Test: `confirm_message_names_chunks_never_bytes`,
 * `confirm_message_flags_an_unknown_chunk_count`.
 *
 * @param {string[]} ids The ids about to be deleted.
 * @param {boolean} deleteData Whether the on-disk corpus goes too.
 * @param {(number|null)[]} chunkCounts One count per id, in the same order.
 * @returns {string} The confirm sentence.
 */
export function pruneConfirmMessage(ids, deleteData, chunkCounts = []) {
  const n = ids.length;
  const noun = n === 1 ? 'registration' : 'registrations';
  const fate = deleteData
    ? 'Their on-disk index data will be deleted too.'
    : 'Their on-disk index data will be left in place.';
  const known = chunkCounts.filter((c) => typeof c === 'number');
  const unknown = chunkCounts.length - known.length;
  const total = known.reduce((sum, c) => sum + c, 0);
  const parts = [`Remove ${n} stale ${noun}?`];
  if (chunkCounts.length > 0) {
    const tail = unknown > 0 ? `, and ${unknown} whose chunk count the daemon did not report` : '';
    parts.push(`They hold ${total.toLocaleString('en-US')} indexed chunks${tail}.`);
  }
  parts.push(fate, 'This cannot be undone.');
  return parts.join(' ');
}

/**
 * Where a reviewed unjudged row's data sits, and that it stays.
 *
 * Why (#6423): the confirmation used to assert "there is no index data to
 * delete" for every row, and that is false twice over. A `colocated` row keeps
 * its data BESIDE the root, and the daemon put the row in `indeterminate`
 * precisely because it could not tell whether that root is gone — an unmounted
 * volume's data is still there. A non-colocated row's data sits in
 * trusty-search's own data directory, plainly on disk. Neither is deleted here
 * and neither is absent.
 *
 * @param {{colocated?: boolean}} row The reviewed registration.
 * @returns {string} A clause naming where the data is and that it stays.
 */
function unjudgedDataFate(row) {
  return row && row.colocated === true
    ? 'Its index data sits beside that root and is left untouched — the root could not ' +
        'be reached to check, so the data may well still be there.'
    : "Its index data sits in trusty-search's own directory and is left untouched — only " +
        'the registration is removed.';
}

/**
 * The sentence the per-row deregister confirmation shows.
 *
 * Why (#6423): this is the only confirm step for a destructive action on a row
 * the daemon could not check, so it names the PATH rather than the count. There
 * is no batch to summarise, and the path is what distinguishes six
 * otherwise-identical rows under one retired parent directory.
 *
 * @param {{id: string, root_path: string, colocated?: boolean}} row
 * @returns {string} The confirm sentence.
 */
export function unjudgedConfirmMessage(row) {
  const id = row && typeof row.id === 'string' ? row.id : '';
  const path = row && typeof row.root_path === 'string' ? row.root_path : '(unknown path)';
  return (
    `Deregister "${id}"? Its root ${path} could not be checked. ` +
    `${unjudgedDataFate(row)} This cannot be undone.`
  );
}

/**
 * The same fate, for the review panel's note above the two buttons.
 *
 * Exported so the panel and the confirmation cannot drift apart — they are two
 * renderings of one fact, and the #6423 round-2 defect was exactly a note and a
 * confirmation disagreeing about it.
 *
 * @param {{colocated?: boolean}} row The reviewed registration.
 * @returns {string} The note.
 */
export function unjudgedReviewNote(row) {
  return `Keeping leaves the registration exactly as it is. ${unjudgedDataFate(row)}`;
}

/**
 * What one `DELETE /indexes/{id}` actually did, read from its BODY.
 *
 * Why (#6363, and the #4846 no-op this rule comes from): the daemon answers
 * `removed: false` when it removed nothing — an id present in no store and no
 * `indexes.toml` row answers `404 {removed:false}`, and a delete whose durable
 * cleanup failed answers `500 {ok:false}` with the registration still in the
 * file. A caller that reads the status code, or that assumes a delete it sent
 * happened, records a removal that did not — which leaves the operator believing
 * a registration is gone while it keeps `warm_boot_degraded` true. So: success
 * is `ok === true` AND `removed === true`, and anything else is reported with
 * the daemon's own words.
 *
 * What: a verdict plus the sentence to show. `removed` is echoed as the daemon
 * stated it so the panel can say "not removed" rather than "failed", which are
 * different things to an operator.
 * Test: `a_removed_false_answer_is_never_success`,
 * `an_unloaded_registration_reports_the_daemon_answer_verbatim`,
 * `a_failed_durable_cleanup_is_not_a_removal`,
 * `a_body_that_did_not_parse_names_the_status`.
 *
 * @param {number} httpStatus HTTP status of the daemon's response.
 * @param {object|null} body Parsed JSON body, or null when it did not parse.
 * @returns {{ok: boolean, removed: boolean, dataDeleted: boolean, message: string}}
 */
export function readDeleteOutcome(httpStatus, body) {
  const id = body && typeof body.id === 'string' ? body.id : '';
  const daemonError = body && typeof body.error === 'string' ? body.error.trim() : '';
  const removed = Boolean(body && body.removed === true);
  const ok = Boolean(body && body.ok === true) && removed;
  const dataDeleted = Boolean(body && body.data_deleted === true);

  if (ok) {
    const fate = dataDeleted
      ? 'Its on-disk index data was deleted.'
      : 'Its on-disk index data was left in place.';
    return { ok: true, removed: true, dataDeleted, message: `Removed "${id}". ${fate}` };
  }

  if (body && Object.prototype.hasOwnProperty.call(body, 'removed')) {
    const head = removed
      ? `"${id}" was deregistered but the delete did not finish`
      : `"${id}" was NOT removed — trusty-search answered removed: false`;
    const tail = daemonError ? `: ${daemonError}` : ` (HTTP ${httpStatus}).`;
    return { ok: false, removed, dataDeleted, message: `${head}${tail}` };
  }

  return {
    ok: false,
    removed: false,
    dataDeleted: false,
    message:
      daemonError ||
      `The delete failed (HTTP ${httpStatus}) and trusty-search gave no reason.`
  };
}

/**
 * Whether a batch of per-id outcomes fully succeeded, and what to say about it.
 *
 * Why (#6371): a batch has no single outcome, and reporting one is the failure
 * this panel exists to avoid — three ids removed and one refused is not
 * "cleaned". The dashboard sends one DELETE per id, so the rows are its own; this
 * only counts them.
 *
 * @param {{id: string, ok: boolean, message: string}[]} rows One row per id.
 * @returns {{ok: boolean, removed: number, failed: number, message: string}}
 */
export function summarizeBatch(rows) {
  const list = Array.isArray(rows) ? rows : [];
  const removed = list.filter((r) => r && r.ok === true).length;
  const failed = list.length - removed;
  if (list.length === 0) {
    return { ok: false, removed: 0, failed: 0, message: 'Nothing was attempted.' };
  }
  if (failed === 0) {
    return {
      ok: true,
      removed,
      failed,
      message: `Removed ${removed} stale registration${removed === 1 ? '' : 's'}.`
    };
  }
  return {
    ok: false,
    removed,
    failed,
    message: `Removed ${removed}; ${failed} could not be removed. Each is listed below with trusty-search's own answer.`
  };
}
