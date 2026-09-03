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

import { windowMax } from './barGraph.js';
import { cardPresentation } from './statusPresentation.js';

/** The roster route both the home page and the screensaver read (#6643). */
export const SERVICES_URL = '/api/console/services';

/** The placeholder for an absent version or an unmeasurable CPU figure. */
export const DASH = '—';

/** What a row without a dashboard says when hovered or read aloud. */
export const NO_DASHBOARD_HINT = 'No dashboard for this service';

/** True only for a real, finite number — `null`, `undefined` and NaN are not. */
function isNum(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

/**
 * Read the service roster, never rejecting (#6643).
 *
 * Why it lives here: two callers need the same roster — the home page for the
 * list it renders, and the screensaver, which re-reads it on its own poll so a
 * screen left up for hours notices a service that was installed or removed
 * while nobody was watching. One wrapper is also what keeps a failed read
 * shaped the same for both: an empty roster and a stated error, so neither
 * caller has to decide what an exception means.
 *
 * What: resolves to `{ services, error }`. `error` is `null` on success, and on
 * failure `services` is empty — the caller keeps whatever it was already
 * showing if it would rather not blank the screen.
 * Test: `fetchServices returns the roster on a 200`, `fetchServices reports a
 * non-OK status without throwing`, `fetchServices reports a transport failure`.
 *
 * @param {typeof fetch} fetchImpl injected so `node --test` needs no browser
 * @returns {Promise<{ services: object[], error: string | null }>}
 */
export async function fetchServices(fetchImpl = fetch) {
  try {
    const resp = await fetchImpl(SERVICES_URL);
    if (!resp?.ok) return { services: [], error: `HTTP ${resp?.status}` };
    const body = await resp.json();
    return { services: Array.isArray(body) ? body : [], error: null };
  } catch (e) {
    return { services: [], error: e?.message ?? String(e) };
  }
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
 * The complete accessible name for a clickable row (#6642).
 *
 * Why: `aria-label` REPLACES the name a screen reader would otherwise assemble
 * from the row's cells, so a label naming only the service and the action left
 * a listener with no version, no status and no CPU figure — every column the
 * row exists to carry. A row that navigates needs its action named, so the
 * label stays and says everything instead.
 *
 * What: one sentence in the order the columns are read, with the dash
 * placeholder spelled out — "em dash" announced for a missing version tells the
 * listener nothing.
 * Test: `a clickable row's accessible name carries every column`,
 * `an absent version and an unmeasured CPU are spelled out, not dashes`.
 */
export function rowAriaLabel(row) {
  const version = row.version === DASH ? 'version unknown' : `version ${row.version}`;
  const cpu = row.cpuLabel === DASH ? 'CPU not measured' : `${row.cpuLabel} CPU`;
  return `${row.displayName}, ${version}, ${row.statusLabel}, ${cpu} — open dashboard`;
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

    const row = {
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
    // #6642: only a clickable row overrides its accessible name. An inert row
    // is a plain <div>, whose visible cells a screen reader reads as written.
    return { ...row, ariaLabel: row.hasDashboard ? rowAriaLabel(row) : null };
  });
}

/**
 * The %CPU floor a row's bars are scaled against.
 *
 * A window of near-idle samples would otherwise magnify 0.2 % into a full-height
 * bar, which reads as a busy daemon. Five percent is the smallest ceiling that
 * still leaves a genuinely idle service looking idle.
 */
export const ROW_GRAPH_FLOOR_PCT = 5;

/**
 * Everything `BarGraph` needs to draw one service row's CPU graph (#6643).
 *
 * Why here: the home page's list and the screensaver's table draw the same row
 * for the same service, and a per-row scale is only comparable if both compute
 * it the same way. A shared scale across rows would instead flatten every row
 * against whichever service happens to be compiling; per-row scaling answers
 * "is this daemon busier than it was a minute ago", which is the question a
 * row-height graph can actually answer. The absolute figure stays in the %CPU
 * column.
 *
 * What: the row's own series, scaled to the busiest second in it, never below
 * [`ROW_GRAPH_FLOOR_PCT`]. No thresholds — a busy daemon is not a fault.
 * Test: `rowGraphSpec scales a row to its own busiest second`, `rowGraphSpec
 * holds an idle row against the floor`.
 *
 * @param {{ series?: (number|null)[], displayName?: string }} row from `serviceRows`
 * @returns {{ values: (number|null)[], max: number, label: string }}
 */
export function rowGraphSpec(row) {
  const series = Array.isArray(row?.series) ? row.series : [];
  return {
    values: series,
    max: windowMax(series, ROW_GRAPH_FLOOR_PCT),
    label: `${row?.displayName ?? 'Service'} CPU, one bar per second`,
  };
}

/**
 * The order status labels are tallied in — worst last, so the eye lands on it.
 *
 * A label this list does not name is a status a newer daemon added; it sorts
 * after these, alphabetically, rather than being dropped.
 */
export const STATUS_LABEL_ORDER = ['Running', 'Ready', 'Available', 'Degraded', 'Absent'];

/**
 * One tally per status label present in the rows (#6643).
 *
 * Why: the screensaver shows a count line on one frame and the list on another,
 * and a count computed from a different payload than the list is how the two
 * frames end up disagreeing about how many services exist. Tallying the rows
 * themselves makes that impossible, and reusing their `statusLabel` and
 * `statusVar` means the tally says the same words in the same colours the
 * badges beneath it do.
 *
 * What: labels with a zero count are absent from the result — the line names
 * what is there, not every state a service could be in.
 * Test: `statusCounts tallies the rows by the label they display`,
 * `statusCounts orders known labels by severity and unknown ones last`.
 *
 * @param {{ statusLabel: string, statusVar: string }[]} rows from `serviceRows`
 * @returns {{ label: string, count: number, toneVar: string }[]}
 */
export function statusCounts(rows) {
  const byLabel = new Map();
  for (const row of rows ?? []) {
    const label = row?.statusLabel ?? '';
    const seen = byLabel.get(label);
    if (seen) seen.count += 1;
    else byLabel.set(label, { label, count: 1, toneVar: row?.statusVar ?? '' });
  }
  const rank = (label) => {
    const index = STATUS_LABEL_ORDER.indexOf(label);
    return index === -1 ? STATUS_LABEL_ORDER.length : index;
  };
  return [...byLabel.values()].sort((a, b) => {
    if (rank(a.label) !== rank(b.label)) return rank(a.label) - rank(b.label);
    return a.label.localeCompare(b.label);
  });
}
