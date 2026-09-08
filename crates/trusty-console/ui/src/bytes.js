/**
 * One byte formatter for the whole console panel (#6928).
 *
 * Why: the Search tab carried a private `formatBytes` for its index sizes, and
 * the Memory tab now needs the identical thing for palace disk and daemon RAM.
 * A second copy is how the two tabs start disagreeing about where the KB/MB
 * boundary sits — and a third was about to be written.
 *
 * What: one pure function, no DOM, no fetch.
 * Test: `bytes.test.js` — run `node --test src/bytes.test.js` from
 * `crates/trusty-console/ui`.
 */

/** What a size cell shows when the figure could not be read. */
export const UNKNOWN_SIZE = '—';

/**
 * Human-readable byte size: `B` under a kibibyte, then KB / MB / GB.
 *
 * A non-number — `null`, `undefined`, a string — is a figure that could not be
 * read, and renders as [`UNKNOWN_SIZE`] rather than as `0 B`. Those are
 * different facts: an operator reading `0 B` about a 1.5 GB palace store would
 * be reading a bug as a measurement.
 *
 * @param {unknown} bytes byte count, or a non-number for "unknown"
 * @returns {string} formatted size
 */
export function formatBytes(bytes) {
  if (typeof bytes !== 'number' || !Number.isFinite(bytes) || bytes < 0) {
    return UNKNOWN_SIZE;
  }
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}
