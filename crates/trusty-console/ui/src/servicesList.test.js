/**
 * Tests for the home page's Services list data layer (#6642).
 *
 * Run: `node --test src/servicesList.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  DASH,
  ROW_GRAPH_FLOOR_PCT,
  SERVICES_URL,
  cpuSeries,
  fetchServices,
  formatCpu,
  latestSample,
  rowAriaLabel,
  rowGraphSpec,
  serviceRows,
  sortByDisplayName,
  statusCounts,
} from './servicesList.js';

const ROSTER = [
  { id: 'trusty-search', display_name: 'Trusty Search', status: 'running', version: '0.41.0', cpu_pct: 2.5 },
  { id: 'trusty-agents', display_name: 'trusty agents', status: 'running', version: '0.3.0', cpu_pct: null },
  { id: 'trusty-memory', display_name: 'Trusty Memory', status: 'running', version: '0.9.1', cpu_pct: 0 },
  { id: 'trusty-analyze', display_name: 'Trusty Analyze', status: 'available', lifecycle: 'on_demand', cpu_pct: null },
];

test('the list is alphabetical by display name, case-insensitive', () => {
  const names = sortByDisplayName(ROSTER).map((s) => s.display_name);
  assert.deepEqual(names, [
    'trusty agents',
    'Trusty Analyze',
    'Trusty Memory',
    'Trusty Search',
  ]);
});

test('two identical display names fall back to the id for a stable order', () => {
  const sorted = sortByDisplayName([
    { id: 'b', display_name: 'Same' },
    { id: 'a', display_name: 'Same' },
  ]);
  assert.deepEqual(sorted.map((s) => s.id), ['a', 'b']);
});

test('a CPU figure is one decimal; an unmeasurable one is a dash, never zero', () => {
  assert.equal(formatCpu(2.5), '2.5%');
  assert.equal(formatCpu(0), '0.0%', 'a measured idle daemon reads as zero');
  assert.equal(formatCpu(null), DASH);
  assert.equal(formatCpu(undefined), DASH);
  assert.equal(formatCpu(NaN), DASH);
});

test('a null cpu_pct survives into the series as a gap', () => {
  const samples = {
    'trusty-search': [
      { id: 'trusty-search', status: 'running', cpu_pct: 1 },
      { id: 'trusty-search', status: 'running', cpu_pct: null },
      { id: 'trusty-search', status: 'running', cpu_pct: 3 },
    ],
  };
  assert.deepEqual(cpuSeries(samples, 'trusty-search'), [1, null, 3]);
  assert.deepEqual(cpuSeries(samples, 'nobody'), []);
});

test('latestSample reads the newest entry, which is last', () => {
  const samples = { x: [{ cpu_pct: 1 }, { cpu_pct: 9 }] };
  assert.equal(latestSample(samples, 'x').cpu_pct, 9);
  assert.equal(latestSample(samples, 'y'), null);
});

test('a row renders version, status and CPU, and a missing version is a dash', () => {
  const rows = serviceRows(ROSTER, {}, new Set(['trusty-search']));
  const search = rows.find((r) => r.id === 'trusty-search');
  const analyze = rows.find((r) => r.id === 'trusty-analyze');

  assert.equal(search.version, '0.41.0');
  assert.equal(search.statusLabel, 'Running');
  assert.equal(search.cpuLabel, '2.5%');
  assert.equal(analyze.version, DASH, 'no version reported → dash');
  assert.equal(analyze.cpuLabel, DASH, 'an on-demand member has no CPU to show');
  assert.equal(analyze.statusLabel, 'Ready', 'on-demand at rest is ready, not a warning');
});

test('the newest stream sample wins over the roster snapshot', () => {
  const samples = {
    'trusty-search': [
      { id: 'trusty-search', status: 'running', cpu_pct: 2.5 },
      { id: 'trusty-search', status: 'degraded', cpu_pct: 41.25 },
    ],
  };
  const [row] = serviceRows(
    [ROSTER[0]],
    samples,
    new Set(),
  );
  assert.equal(row.cpuLabel, '41.3%', 'the live figure, rounded — not the fetched 2.5');
  assert.equal(row.statusLabel, 'Degraded');
  assert.deepEqual(row.series, [2.5, 41.25]);
});

test('only a service with a dashboard is marked clickable', () => {
  const rows = serviceRows(ROSTER, {}, new Set(['trusty-search', 'trusty-memory']));
  const clickable = rows.filter((r) => r.hasDashboard).map((r) => r.id);
  assert.deepEqual(clickable.sort(), ['trusty-memory', 'trusty-search']);
  assert.equal(rows.find((r) => r.id === 'trusty-agents').hasDashboard, false);
});

// ── #6642: the clickable row's accessible name ─────────────────────────────

test("a clickable row's accessible name carries every column", () => {
  const rows = serviceRows(ROSTER, {}, new Set(['trusty-search']));
  const search = rows.find((r) => r.id === 'trusty-search');
  assert.equal(
    search.ariaLabel,
    'Trusty Search, version 0.41.0, Running, 2.5% CPU — open dashboard',
  );
  // Every column a sighted reader sees survives into the name the aria-label
  // replaces — that replacement is the whole hazard.
  for (const part of [search.version, search.statusLabel, search.cpuLabel]) {
    assert.ok(search.ariaLabel.includes(part), `label omits ${part}`);
  }
});

test('an absent version and an unmeasured CPU are spelled out, not dashes', () => {
  const label = rowAriaLabel({
    displayName: 'Trusty Analyze',
    version: DASH,
    statusLabel: 'Ready',
    cpuLabel: DASH,
  });
  assert.equal(label, 'Trusty Analyze, version unknown, Ready, CPU not measured — open dashboard');
  assert.ok(!label.includes(`${DASH},`), 'no bare dash cell survives into the name');
});

test('an inert row overrides no accessible name at all', () => {
  const rows = serviceRows(ROSTER, {}, new Set(['trusty-search']));
  assert.equal(rows.find((r) => r.id === 'trusty-agents').ariaLabel, null);
});

test('an empty roster yields no rows rather than throwing', () => {
  assert.deepEqual(serviceRows(undefined), []);
  assert.deepEqual(serviceRows([]), []);
});

// ── shared with the screensaver (#6643) ────────────────────────────────────

test('rowGraphSpec scales a row to its own busiest second', () => {
  const spec = rowGraphSpec({ displayName: 'Trusty Search', series: [1, 12.5, null, 4] });
  assert.equal(spec.max, 12.5);
  assert.deepEqual(spec.values, [1, 12.5, null, 4]);
  assert.equal(spec.label, 'Trusty Search CPU, one bar per second');
});

test('rowGraphSpec holds an idle row against the floor', () => {
  // Without the floor a window of 0.2 % samples draws full-height bars and an
  // idle daemon reads as a busy one.
  assert.equal(rowGraphSpec({ series: [0.2, 0.1] }).max, ROW_GRAPH_FLOOR_PCT);
  assert.equal(rowGraphSpec({}).max, ROW_GRAPH_FLOOR_PCT);
  assert.deepEqual(rowGraphSpec(undefined).values, []);
});

test('statusCounts tallies the rows by the label they display', () => {
  const counts = statusCounts(serviceRows(ROSTER, {}, new Set()));
  assert.deepEqual(counts, [
    { label: 'Running', count: 3, toneVar: 'var(--trusty-success)' },
    { label: 'Ready', count: 1, toneVar: 'var(--trusty-success)' },
  ]);
  // Every row is counted exactly once, so the tally and the list can never
  // report different populations.
  assert.equal(
    counts.reduce((sum, c) => sum + c.count, 0),
    ROSTER.length,
  );
});

test('statusCounts orders known labels by severity and unknown ones last', () => {
  const counts = statusCounts([
    { statusLabel: 'Absent', statusVar: 'var(--trusty-status-absent)' },
    { statusLabel: 'Hibernating', statusVar: 'var(--trusty-text-muted)' },
    { statusLabel: 'Degraded', statusVar: 'var(--trusty-status-degraded)' },
    { statusLabel: 'Running', statusVar: 'var(--trusty-success)' },
  ]);
  assert.deepEqual(
    counts.map((c) => c.label),
    ['Running', 'Degraded', 'Absent', 'Hibernating'],
  );
  assert.deepEqual(statusCounts(undefined), []);
});

test('fetchServices returns the roster on a 200', async () => {
  let asked = null;
  const result = await fetchServices(async (url) => {
    asked = url;
    return { ok: true, status: 200, json: async () => ROSTER };
  });
  assert.equal(asked, SERVICES_URL);
  assert.equal(result.error, null);
  assert.equal(result.services.length, ROSTER.length);
});

test('fetchServices reports a failure without throwing', async () => {
  // Both callers poll on a timer, where a rejected promise is unhandled.
  const notOk = await fetchServices(async () => ({ ok: false, status: 503 }));
  assert.deepEqual(notOk, { services: [], error: 'HTTP 503' });

  const threw = await fetchServices(async () => {
    throw new Error('network down');
  });
  assert.deepEqual(threw, { services: [], error: 'network down' });

  // A body that is not an array is a build mismatch, not a roster.
  const wrongShape = await fetchServices(async () => ({
    ok: true,
    status: 200,
    json: async () => ({ services: [] }),
  }));
  assert.deepEqual(wrongShape, { services: [], error: null });
});
