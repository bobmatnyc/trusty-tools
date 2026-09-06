/**
 * Where a Search-tab index row navigates (#6923).
 *
 * Why: DOC-73 §13 makes the console display-only — the Search tab shows the
 * index roster and nothing else — and §14 binds every element-list row to that
 * element's management view. For an index that view is the search dashboard the
 * console already serves at `/tools/search/`, whose per-index screen carries the
 * create, reindex, delete and hygiene controls the tab used to duplicate.
 *
 * What: builds the href a row links to. The dashboard is a HASH-routed SPA
 * (`ui-search/src/lib/router.svelte.js`), and its per-index route is
 * `#/indexes/<id>/config` (`ui-search/src/App.svelte:44-51`) — the console
 * serves no `/tools/search/indexes/<id>` path, so that is the route #6923's
 * "row-click to /tools/search/indexes" resolves to.
 *
 * Test: `searchIndexNav.test.js` — run `node --test src/searchIndexNav.test.js`
 * from `crates/trusty-console/ui`.
 */

/** The search dashboard's mount point in the console (`tools_ui.rs`). */
export const SEARCH_DASHBOARD_URL = '/tools/search/';

/** What a row with no id says when hovered or read aloud. */
export const NO_INDEX_ID_HINT = 'This registration reports no id, so it has no management view';

/**
 * The management view for one index.
 *
 * The id is percent-encoded: an index id is a free-form string, and one
 * carrying a `/` or a `#` would otherwise land on a different route.
 *
 * @param {string} id the index id from the roster
 * @returns {string} an href under the console's search dashboard
 */
export function indexDashboardHref(id) {
  return `${SEARCH_DASHBOARD_URL}#/indexes/${encodeURIComponent(id)}/config`;
}

/**
 * The complete accessible name for a clickable index row.
 *
 * Why it restates every cell: `aria-label` REPLACES the name a screen reader
 * assembles from the row's contents, so a label naming the action alone would
 * cost a listener the root path, the size and the last-used figure the row
 * exists to carry — the same rule `servicesList.js`'s `rowAriaLabel` holds.
 *
 * @param {{id: string, rootPath: string, size: string, lastUsed: string}} cells
 *        already-formatted cell text, so the label says what the row shows
 */
export function indexRowAriaLabel({ id, rootPath, size, lastUsed }) {
  return `Index ${id}, ${rootPath}, ${size}, last used ${lastUsed} — open index management`;
}
