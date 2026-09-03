/**
 * The home page's Services list, as data (#6642).
 *
 * Why: the owner replaced the "Installed Services" card grid with one
 * alphabetical list carrying version, status, %CPU and a per-row CPU graph. All
 * four columns are projections of two payloads — `GET /api/console/services`
 * for the roster, and the machine-status stream's per-service rings for the
 * live figures — and every one of the rules worth getting wrong (the sort, the
 * dash-not-zero rule, which rows are clickable) is a pure function. Keeping
 * them here makes them testable under `node --test`, leaving
 * `ServicesList.svelte` as markup.
 *
 * What: `serviceRows` is the one entry point the component calls; the rest are
 * its pieces, exported because each is worth asserting on its own.
 *
 * Three rules this module exists to hold:
 *   - Sort is case-insensitive on `display_name`, with the id as tie-break, so
 *     the order is stable across renders and does not depend on the order the
 *     server happened to send.
 *   - `cpu_pct` of `null` renders a dash, never `0.0`. An on-demand member with
 *     no resident process and a daemon idling at zero are different facts
 *     (#6642 acceptance).
 *   - The newest stream sample wins over the roster's snapshot. The roster is
 *     fetched once for first paint; after that the 1 Hz `services` event is the
 *     fresher number, and showing the stale one beside a live graph would be a
 *     visible contradiction.
 *
 * Test: `servicesList.test.js` — run `node --test src/servicesList.test.js`
 * from `crates/trusty-console/ui`.
 */

import { cardPresentation } from './statusPresentation.js';

/** The placeholder for an absent version or an unmeasurable CPU figure. */
export const DASH = '—';

/** What a row without a dashboard says when hovered or read aloud. */
export const NO_DASHBOARD_HINT = 'No dashboard for this service';

/** True only for a real, finite number — `null`, `undefined` and NaN are not. */
function isNum(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

/**
 * Alphabetical by display name, case-insensitive, id as tie-break.
 *
 * Returns a new array; the caller's roster is never reordered in place.
 */
export function sortByDisplayName(services) {
  return [...(services ?? [])].sort((a, b) => {
    const left = (a?.display_name ?? a?.id ?? '').toLowerCase();
    const right = (b?.display_name ?? b?.id ?? '').toLowerCase();
    if (left !== right) return left < right ? -1 : 1;
    return (a?.id ?? '').localeCompare(b?.id ?? '');
  });
}

/** A CPU percentage to one decimal, or the dash when nothing was measured. */
export function formatCpu(cpuPct) {
  return isNum(cpuPct) ? `${cpuPct.toFixed(1)}%` : DASH;
}

/**
 * One service's CPU history, oldest first, `null` where nothing was measured.
 *
 * The nulls are preserved rather than filtered: `BarGraph` draws them as gaps,
 * and dropping them would slide unrelated seconds together into a graph that
 * claims continuity it does not have.
 */
export function cpuSeries(serviceSamples, id) {
  const list = serviceSamples?.[id];
  if (!Array.isArray(list)) return [];
  return list.map((sample) => (isNum(sample?.cpu_pct) ? sample.cpu_pct : null));
}

/** The newest per-service sample the stream delivered, or `null`. */
export function latestSample(serviceSamples, id) {
  const list = serviceSamples?.[id];
  return Array.isArray(list) && list.length > 0 ? list[list.length - 1] : null;
}

/**
 * Every row the list renders, in display order.
 *
 * @param {object[]} services the `/api/console/services` roster
 * @param {object} serviceSamples the stream's per-service rings, keyed by id
 * @param {Set<string>} dashboards ids that have a dashboard to open
 */
export function serviceRows(services, serviceSamples = {}, dashboards = new Set()) {
  return sortByDisplayName(services).map((service) => {
    const live = latestSample(serviceSamples, service.id);
    // The stream's status is the same health the roster reports, sampled more
    // recently; the rest of the row (version, lifecycle, hint) only the roster
    // has, so the two are merged rather than swapped.
    const merged = live ? { ...service, status: live.status } : service;
    const presentation = cardPresentation(merged);
    const cpu = live ? live.cpu_pct : service.cpu_pct;

    return {
      id: service.id,
      displayName: service.display_name || service.id,
      version: service.version || DASH,
      status: merged.status,
      statusLabel: presentation.label,
      statusVar: presentation.toneVar,
      cpuLabel: formatCpu(cpu),
      series: cpuSeries(serviceSamples, service.id),
      hasDashboard: dashboards.has(service.id),
    };
  });
}
