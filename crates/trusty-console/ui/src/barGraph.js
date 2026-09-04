/**
 * The bar-graph geometry every home-page card and service row draws (#6642).
 *
 * Why a module rather than markup: the owner's ruling puts a bar graph on the
 * bottom edge of every card AND on every service row — ten graphs on one screen,
 * each redrawn once a second. Rendering 600 `<rect>` elements per graph would
 * put 6000 nodes on the page and rewrite every one of their attributes every
 * tick. One `<path>` per colour band draws the same bars from three strings, so
 * a tick rewrites three attributes instead of thousands. Keeping the string
 * building here makes that geometry testable under `node --test`.
 *
 * What: pure functions producing SVG path data in a `slots × HEIGHT_UNITS`
 * coordinate space. The graph is drawn with `preserveAspectRatio="none"`, so
 * one unit of x is one sample whatever the card's rendered width is, and the
 * caller never computes a pixel.
 *
 * Two rules:
 *   - `null` is a gap, never a zero-height bar. An unmeasurable service and an
 *     idle one must not look the same (#6642 acceptance).
 *   - Thresholds are the server's bands, restated. The stat cards already
 *     colour against 80/95 (disk 85/95) via `Pressure`; the bars reuse those
 *     numbers so a card whose badge says WARNING cannot have bars that say
 *     otherwise. Nothing here invents a band.
 *
 * Test: `barGraph.test.js` — run `node --test src/barGraph.test.js` from
 * `crates/trusty-console/ui`.
 */

/** Height of the drawing space, in viewBox units. */
export const HEIGHT_UNITS = 100;

/** Fraction of a one-sample slot the bar itself occupies; the rest is the gap. */
export const BAR_WIDTH = 0.78;

/**
 * Shortest bar drawn for a measured value, in viewBox units.
 *
 * A 0.1 % CPU sample scales to a tenth of a unit and disappears, which reads as
 * the gap that `null` is reserved for. A floor keeps "measured and near zero"
 * visibly different from "not measured".
 */
export const MIN_BAR_UNITS = 2;

/**
 * The pressure bands the four host cards colour against.
 *
 * These mirror `HostThresholds::default()` in
 * `crates/trusty-common/src/host_metrics.rs`. Disk warns later than CPU and
 * memory there, and does here.
 */
export const PCT_THRESHOLDS = { warning: 80, critical: 95 };
export const DISK_THRESHOLDS = { warning: 85, critical: 95 };

/** True only for a real, finite number — `null`, `undefined` and NaN are not. */
function isNum(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

/**
 * The largest finite value in `values`, or `floor` when there is none.
 *
 * Why it exists: the network card has no percentage to scale against, so its
 * bars are scaled to the busiest second currently on screen. `floor` keeps a
 * window of all-zeros from dividing by zero and from magnifying noise.
 */
export function windowMax(values, floor = 1) {
  let max = floor;
  for (const value of values ?? []) {
    if (isNum(value) && value > max) max = value;
  }
  return max;
}

/**
 * Which colour band a value falls in: `'nominal' | 'warning' | 'critical'`.
 *
 * `thresholds` of `null` means the series has no bands — a throughput rate is
 * not a level — and every bar is nominal.
 */
export function toneFor(value, thresholds) {
  if (!thresholds || !isNum(value)) return 'nominal';
  if (isNum(thresholds.critical) && value >= thresholds.critical) return 'critical';
  if (isNum(thresholds.warning) && value >= thresholds.warning) return 'warning';
  return 'nominal';
}

/**
 * SVG path data for one bar graph, split into one path per colour band.
 *
 * `values` are oldest-first; the newest sample is the rightmost bar. The
 * viewBox width is the sample count, so the window spans the full card at every
 * size — a graph one minute old shows sixty fat bars and the same graph at ten
 * minutes shows six hundred thin ones.
 *
 * @param {(number|null)[]} values oldest first
 * @param {{ max?: number, thresholds?: object|null }} options
 * @returns {{ slots: number, height: number, paths: { nominal: string, warning: string, critical: string }, drawn: number }}
 */
export function barPaths(values, { max = 100, thresholds = null } = {}) {
  const list = Array.isArray(values) ? values : [];
  const slots = Math.max(list.length, 1);
  const scale = isNum(max) && max > 0 ? max : 1;
  const paths = { nominal: '', warning: '', critical: '' };
  let drawn = 0;

  for (let i = 0; i < list.length; i += 1) {
    const value = list[i];
    // A null is a gap: no bar is emitted at this slot at all.
    if (!isNum(value)) continue;
    const ratio = Math.min(Math.max(value / scale, 0), 1);
    const height = Math.max(ratio * HEIGHT_UNITS, MIN_BAR_UNITS);
    const y = HEIGHT_UNITS - height;
    const x = i + (1 - BAR_WIDTH) / 2;
    paths[toneFor(value, thresholds)] +=
      `M${round(x)} ${round(y)}h${BAR_WIDTH}v${round(height)}h-${BAR_WIDTH}z`;
    drawn += 1;
  }

  return { slots, height: HEIGHT_UNITS, paths, drawn };
}

/** Three decimals is below one device pixel at any card width, and keeps the
 *  `d` attribute short enough that a 600-bar rewrite stays cheap. */
function round(value) {
  return Number(value.toFixed(3));
}
