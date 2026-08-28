/**
 * How one palace row reads on the Memory tab (#6372).
 *
 * Why: the tab used to branch on `cached === false` and print `—` for every
 * count, which on a host with 94 palaces and 2 resident meant 92 populated
 * palaces looked empty. trusty-memory now reports real counts for a closed
 * palace too, and says which of three ways it got them, so the row has to
 * branch on `stats_source` instead. Keeping that decision here — rather than
 * inline in the template — is what makes it testable without a browser, and
 * keeps the "unknown" case from silently becoming the "zero" case again.
 *
 * What: two pure functions over one entry of `report.metrics.palaces`. No
 * fetch, no DOM.
 * Test: `palaceRows.test.js` — run `node --test src/palaceRows.test.js` from
 * `crates/trusty-console/ui`.
 */

/** What the cell shows when a count could not be read at all. */
export const UNKNOWN = '—';

/**
 * Where a row's counts came from, normalised across schema versions.
 *
 * A trusty-memory older than metrics schema 3 sends no `stats_source`; its
 * `cached: false` rows genuinely had no counts, so they map to `unavailable`
 * and keep rendering as they did before rather than claiming a zero.
 *
 * Returns one of `'cache'`, `'disk'`, `'unavailable'`.
 */
export function statsSource(palace) {
  if (!palace) return 'unavailable';
  if (palace.stats_source) return palace.stats_source;
  return palace.cached === false ? 'unavailable' : 'cache';
}

/**
 * The text for one count cell.
 *
 * A number renders as itself, whatever the source — a palace read off disk is
 * as real as one read from the cache. Only a missing count renders as
 * [`UNKNOWN`], and `null` is how trusty-memory spells "could not read this",
 * which is deliberately distinct from `0`.
 */
export function countCell(palace, field) {
  const value = palace?.[field];
  return typeof value === 'number' ? String(value) : UNKNOWN;
}

/**
 * The badge, if any, that explains a row's source.
 *
 * `null` for a cache-resident row: a badge on every row is a badge nobody
 * reads. A disk row is normal and says so quietly; an unavailable row is the
 * only one that carries a warning, and its `title` is the daemon's own reason
 * when it sent one.
 */
export function sourceBadge(palace) {
  switch (statsSource(palace)) {
    case 'cache':
      return null;
    case 'disk':
      return {
        label: 'on disk',
        title: 'Counted from the palace files without opening the palace.',
      };
    default:
      return {
        label: 'unreadable',
        title:
          palace?.stats_error ??
          'Counts could not be read from this palace right now.',
      };
  }
}
