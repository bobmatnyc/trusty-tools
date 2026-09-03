/**
 * Tests for the shared bar-graph geometry (#6642).
 *
 * Run: `node --test src/barGraph.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  DISK_THRESHOLDS,
  HEIGHT_UNITS,
  MIN_BAR_UNITS,
  PCT_THRESHOLDS,
  barPaths,
  toneFor,
  windowMax,
} from './barGraph.js';

test('the viewBox spans one slot per sample so the newest bar is rightmost', () => {
  const { slots, paths } = barPaths([10, 20, 30], { max: 100 });
  assert.equal(slots, 3);
  // The last bar starts just past x=2 — the rightmost slot.
  const starts = [...paths.nominal.matchAll(/M([\d.]+) /g)].map((m) => Number(m[1]));
  assert.equal(starts.length, 3);
  assert.ok(starts[2] > 2 && starts[2] < 3, `last bar at x=${starts[2]}`);
});

test('an empty series still yields one slot rather than a zero-width viewBox', () => {
  const { slots, drawn, paths } = barPaths([], { max: 100 });
  assert.equal(slots, 1);
  assert.equal(drawn, 0);
  assert.equal(paths.nominal, '');
});

test('a null renders as a gap, not a zero-height bar', () => {
  const { drawn } = barPaths([5, null, 5], { max: 100 });
  assert.equal(drawn, 2, 'the null slot emits no bar at all');
});

test('a window of nothing but nulls draws no bar in any band', () => {
  const { drawn, paths, slots } = barPaths([null, null, null], { max: 100 });
  assert.equal(drawn, 0);
  assert.equal(slots, 3, 'the window keeps its width — the samples exist, the values do not');
  assert.equal(paths.nominal, '');
  assert.equal(paths.warning, '');
  assert.equal(paths.critical, '');
});

test('a measured near-zero value keeps a floor so it differs from a gap', () => {
  const { paths } = barPaths([0], { max: 100 });
  assert.match(paths.nominal, new RegExp(`v${MIN_BAR_UNITS}h`), 'a zero sample is still drawn');
});

test('a full-scale value fills the height', () => {
  const { paths } = barPaths([100], { max: 100, thresholds: PCT_THRESHOLDS });
  assert.match(paths.critical, new RegExp(`M[\\d.]+ 0h[\\d.]+v${HEIGHT_UNITS}h`));
});

test('a value above the scale is clamped rather than overflowing the card', () => {
  const { paths } = barPaths([250], { max: 100, thresholds: PCT_THRESHOLDS });
  assert.match(paths.critical, new RegExp(`v${HEIGHT_UNITS}h`));
});

test('bars are split into the same 80/95 bands the stat cards colour against', () => {
  const { paths } = barPaths([10, 85, 97], { max: 100, thresholds: PCT_THRESHOLDS });
  assert.equal(paths.nominal.match(/M/g).length, 1);
  assert.equal(paths.warning.match(/M/g).length, 1);
  assert.equal(paths.critical.match(/M/g).length, 1);
});

test('disk warns at 85, not 80', () => {
  assert.equal(toneFor(82, PCT_THRESHOLDS), 'warning');
  assert.equal(toneFor(82, DISK_THRESHOLDS), 'nominal');
  assert.equal(toneFor(86, DISK_THRESHOLDS), 'warning');
  assert.equal(toneFor(95, DISK_THRESHOLDS), 'critical');
});

test('a series with no bands — a throughput rate — is all one tone', () => {
  const { paths } = barPaths([1, 5_000_000], { max: 5_000_000, thresholds: null });
  assert.equal(paths.warning, '');
  assert.equal(paths.critical, '');
  assert.equal(paths.nominal.match(/M/g).length, 2);
});

test('windowMax scales the network card to the busiest second on screen', () => {
  assert.equal(windowMax([10, 400, 22]), 400);
  assert.equal(windowMax([0, 0, 0]), 1, 'an idle window never divides by zero');
  assert.equal(windowMax([null, undefined, NaN]), 1);
  assert.equal(windowMax(undefined), 1);
});

test('600 samples produce 600 bars', () => {
  const values = Array.from({ length: 600 }, (_, i) => i % 100);
  const { slots, drawn } = barPaths(values, { max: 100 });
  assert.equal(slots, 600);
  assert.equal(drawn, 600);
});
