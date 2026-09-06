/**
 * The console's view model — which views exist and what each one is called
 * (#6909).
 *
 * Why: the top tab bar used to be both the list of views and the labels for
 * them. The owner's ruling removed it — "the top nav is redundant with the
 * service list" — because the Services list already opens every service view.
 * The labels survive their tab bar because the header breadcrumb still has to
 * name the open view, and keeping them in a plain module makes that rule
 * assertable under `node --test`, which cannot mount a Svelte component.
 *
 * What: `VIEW_LABELS` is the whole set of views `App.svelte` renders, keyed by
 * the id it holds in state; `viewLabel` is the breadcrumb's lookup, falling
 * back to the Overview's label rather than rendering `undefined` for a view id
 * that no longer exists.
 *
 * Test: `consoleNav.test.js` — run `node --test src/consoleNav.test.js` from
 * `crates/trusty-console/ui`.
 */

/** The view the console opens on, and the one the breadcrumb returns to. */
const OVERVIEW = 'overview';

/** The one view no Services row reaches — the header action opens it. */
const CONFIG = 'config';

/** Every view the panel renders, keyed by the id `App.svelte` holds in state. */
export const VIEW_LABELS = {
  [OVERVIEW]: 'Overview',
  search: 'Search',
  memory: 'Memory',
  analyze: 'Analyze',
  review: 'Review',
  sessions: 'MPM Sessions', // #6370: UI label only — the id stays 'sessions'
  console: 'Console', // #6908: the console's own details pane
  [CONFIG]: 'Config',
};

/** What the breadcrumb calls `view`; an unknown id reads as the Overview. */
export function viewLabel(view) {
  return VIEW_LABELS[view] ?? VIEW_LABELS[OVERVIEW];
}
