/**
 * Where a Memory-tab palace row navigates (#6928).
 *
 * Why: the owner's ruling makes the console memory tab display-only — it shows
 * the daemon's disk and RAM usage and renders no control that mutates daemon
 * state — and moves compact, delete, re-embed, dream and stop to the memory
 * dashboard the console serves at `/tools/memory/`. A row therefore has to
 * carry the operator TO that dashboard, which is the same rule #6923 applied
 * to the Search tab's index rows.
 *
 * What: builds the href a row links to. The dashboard is a HASH-routed SPA
 * (`ui-memory/src/lib/router.svelte.js`), and its per-palace route is
 * `#/palace/<id>` (`ui-memory/src/App.svelte`) — the console serves no
 * `/tools/memory/palaces/<id>` path, so that is the route the issue's
 * "row-click to /tools/memory/palaces/{id}" resolves to.
 *
 * Test: `palaceNav.test.js` — run `node --test src/palaceNav.test.js` from
 * `crates/trusty-console/ui`.
 */

/** The memory dashboard's mount point in the console (`tools_ui.rs`). */
export const MEMORY_DASHBOARD_URL = '/tools/memory/';

/** What a row with no id says when hovered or read aloud. */
export const NO_PALACE_ID_HINT =
  'This palace reports no id, so it has no management view';

/**
 * The hint a palace row carries when it cannot be opened, or `null`.
 *
 * Why a function rather than an inline `{#if}`: the hint is rendered TWICE on
 * an inert row — as `title` for a pointer and as a visually hidden span for a
 * screen reader — and a decision rendered in two places is one a test should be
 * able to make on its own. Same shape as `searchIndexNav.indexRowHint`.
 * Test: `a row with no id carries the hint, in both places it is rendered`.
 */
export function palaceRowHint(palace) {
  return palace?.id ? null : NO_PALACE_ID_HINT;
}

/**
 * The management view for one palace.
 *
 * The id is percent-encoded: a palace id is a free-form string, and one
 * carrying a `/` or a `#` would otherwise land on a different route.
 *
 * @param {string} id the palace id from the roster
 * @returns {string} an href under the console's memory dashboard
 */
export function palaceDashboardHref(id) {
  return `${MEMORY_DASHBOARD_URL}#/palace/${encodeURIComponent(id)}`;
}

/**
 * The complete accessible name for a clickable palace row.
 *
 * Why it restates every cell: `aria-label` REPLACES the name a screen reader
 * assembles from the row's contents, so a label naming the action alone would
 * cost a listener the counts and the size the row exists to carry — the rule
 * `searchIndexNav.indexRowAriaLabel` and `servicesList.rowAriaLabel` both hold.
 *
 * @param {{name: string, id: string, drawers: string, disk: string, lastUsed: string}} cells
 *        already-formatted cell text, so the label says what the row shows
 */
export function palaceRowAriaLabel({ name, id, drawers, disk, lastUsed }) {
  return `Palace ${name} (${id}), ${drawers} drawers, ${disk} on disk, last used ${lastUsed} — open palace management`;
}
