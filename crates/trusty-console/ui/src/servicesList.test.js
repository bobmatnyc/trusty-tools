/**
 * Tests for the home page's Services list data layer (#6642, #6773).
 *
 * Run: `node --test src/servicesList.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  DASH,
  ROW_GRAPH_FLOOR_PCT,
  ROW_MEMORY_FLOOR_BYTES,
  SERVICES_URL,
  cpuSeries,
  fetchServices,
  formatCpu,
  formatMemory,
  latestSample,
  memorySeries,
  rowAriaLabel,
  rowCpuGraphSpec,
  rowMemoryGraphSpec,
  serviceRows,
  sortByDisplayName,
  statusCounts,
} from './servicesList.js';

/** One mebibyte, so the fixtures below read as the figures the column shows. */
const MIB = 1024 * 1024;

const ROSTER = [
  { id: 'trusty-search', display_name: 'Trusty Search', status: 'running', version: '0.41.0', cpu_pct: 2.5, rss_bytes: 142 * MIB },
  { id: 'trusty-agents', display_name: 'trusty agents', status: 'running', version: '0.3.0', cpu_pct: null, rss_bytes: null },
  { id: 'trusty-memory', display_name: 'Trusty Memory', status: 'running', version: '0.9.1', cpu_pct: 0, rss_bytes: 0 },
  { id: 'trusty-analyze', display_name: 'Trusty Analyze', status: 'available', lifecycle: 'on_demand', cpu_pct: null, rss_bytes: null },
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

test('formatMemory picks the largest unit the figure fills', () => {
  assert.equal(formatMemory(142 * MIB), '142 MiB');
  assert.equal(formatMemory(13.4 * 1024 * MIB), '13.4 GiB');
  assert.equal(formatMemory(4096), '4 KiB');
  assert.equal(formatMemory(512), '512 B');
});

test('formatMemory dashes an absent measurement but not a measured zero', () => {
  // The same rule `formatCpu` holds: an unmeasurable service and a daemon
  // holding almost nothing must not read the same.
  assert.equal(formatMemory(null), DASH);
  assert.equal(formatMemory(undefined), DASH);
  assert.equal(formatMemory(NaN), DASH);
  assert.equal(formatMemory(-1), DASH, 'a negative byte count is not a measurement');
  assert.equal(formatMemory(0), '0 B', 'a measured zero is a number, not a dash');
});

test('a null rss_bytes survives into the memory series as a gap', () => {
  const samples = {
    'trusty-search': [
      { id: 'trusty-search', status: 'running', cpu_pct: 1, rss_bytes: 10 * MIB },
      { id: 'trusty-search', status: 'running', cpu_pct: null, rss_bytes: null },
      { id: 'trusty-search', status: 'running', cpu_pct: 3, rss_bytes: 12 * MIB },
    ],
  };
  assert.deepEqual(memorySeries(samples, 'trusty-search'), [10 * MIB, null, 12 * MIB]);
  assert.deepEqual(memorySeries(samples, 'nobody'), []);
  // REGRESSION (#6773): both graphs read one ring, so the series must have the
  // same length — the bar at index i is the same second in both.
  assert.equal(
    memorySeries(samples, 'trusty-search').length,
    cpuSeries(samples, 'trusty-search').length,
  );
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
  assert.equal(search.memoryLabel, '142 MiB');
  assert.equal(analyze.version, DASH, 'no version reported → dash');
  assert.equal(analyze.cpuLabel, DASH, 'an on-demand member has no CPU to show');
  assert.equal(analyze.memoryLabel, DASH, 'and no memory to show either');
  assert.equal(analyze.statusLabel, 'Ready', 'on-demand at rest is ready, not a warning');
});

test('the newest stream sample wins over the roster snapshot', () => {
  const samples = {
    'trusty-search': [
      { id: 'trusty-search', status: 'running', cpu_pct: 2.5, rss_bytes: 142 * MIB },
      { id: 'trusty-search', status: 'degraded', cpu_pct: 41.25, rss_bytes: 900 * MIB },
    ],
  };
  const [row] = serviceRows(
    [ROSTER[0]],
    samples,
    new Set(),
  );
  assert.equal(row.cpuLabel, '41.3%', 'the live figure, rounded — not the fetched 2.5');
  // #6773: the memory column follows the same rule off the same sample.
  assert.equal(row.memoryLabel, '900 MiB', 'the live figure — not the fetched 142 MiB');
  assert.equal(row.statusLabel, 'Degraded');
  assert.deepEqual(row.series, [2.5, 41.25]);
  assert.deepEqual(row.memorySeries, [142 * MIB, 900 * MIB]);
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
    'Trusty Search, version 0.41.0, Running, 2.5% CPU, 142 MiB memory — open dashboard',
  );
  // Every column a sighted reader sees survives into the name the aria-label
  // replaces — that replacement is the whole hazard.
  for (const part of [search.version, search.statusLabel, search.cpuLabel, search.memoryLabel]) {
    assert.ok(search.ariaLabel.includes(part), `label omits ${part}`);
  }
});

test('an absent version and unmeasured figures are spelled out, not dashes', () => {
  const label = rowAriaLabel({
    displayName: 'Trusty Analyze',
    version: DASH,
    statusLabel: 'Ready',
    cpuLabel: DASH,
    memoryLabel: DASH,
  });
  assert.equal(
    label,
    'Trusty Analyze, version unknown, Ready, CPU not measured, memory not measured — open dashboard',
  );
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

test('rowCpuGraphSpec scales a row to its own busiest second', () => {
  const spec = rowCpuGraphSpec({ displayName: 'Trusty Search', series: [1, 12.5, null, 4] });
  assert.equal(spec.max, 12.5);
  assert.deepEqual(spec.values, [1, 12.5, null, 4]);
  assert.equal(spec.label, 'Trusty Search CPU, one bar per second');
});

test('rowCpuGraphSpec holds an idle row against the floor', () => {
  // Without the floor a window of 0.2 % samples draws full-height bars and an
  // idle daemon reads as a busy one.
  assert.equal(rowCpuGraphSpec({ series: [0.2, 0.1] }).max, ROW_GRAPH_FLOOR_PCT);
  assert.equal(rowCpuGraphSpec({}).max, ROW_GRAPH_FLOOR_PCT);
  assert.deepEqual(rowCpuGraphSpec(undefined).values, []);
});

// ── #6773: the memory graph beside the CPU one ──────────────────────

test('rowMemoryGraphSpec scales a row to its own peak, not to 100', () => {
  // REGRESSION (#6773): a byte count has no percentage to be a percentage OF.
  // Scaling it against 100 draws every daemon's memory as one full-height bar.
  const spec = rowMemoryGraphSpec({
    displayName: 'Trusty Search',
    memorySeries: [400 * MIB, 900 * MIB, null, 600 * MIB],
  });
  assert.equal(spec.max, 900 * MIB);
  assert.deepEqual(spec.values, [400 * MIB, 900 * MIB, null, 600 * MIB]);
  assert.equal(spec.label, 'Trusty Search memory, one bar per second');
});

test('rowMemoryGraphSpec holds a gap-only row against the floor', () => {
  // A window of pure gaps must not divide by zero, and a daemon holding a few
  // kilobytes must not draw as the busiest row on the page.
  assert.equal(rowMemoryGraphSpec({ memorySeries: [null, null] }).max, ROW_MEMORY_FLOOR_BYTES);
  assert.equal(rowMemoryGraphSpec({ memorySeries: [4096] }).max, ROW_MEMORY_FLOOR_BYTES);
  assert.equal(rowMemoryGraphSpec({}).max, ROW_MEMORY_FLOOR_BYTES);
  assert.deepEqual(rowMemoryGraphSpec(undefined).values, []);
});

test('the two row graphs read the same window and each carries its own scale', () => {
  const samples = {
    'trusty-search': [
      { id: 'trusty-search', status: 'running', cpu_pct: 1, rss_bytes: 10 * MIB },
      { id: 'trusty-search', status: 'running', cpu_pct: 2, rss_bytes: 20 * MIB },
      { id: 'trusty-search', status: 'running', cpu_pct: 3, rss_bytes: 30 * MIB },
    ],
  };
  const [row] = serviceRows([ROSTER[0]], samples, new Set());
  const cpu = rowCpuGraphSpec(row);
  const memory = rowMemoryGraphSpec(row);
  assert.equal(cpu.values.length, memory.values.length, 'one x-axis, two graphs');
  assert.equal(cpu.max, ROW_GRAPH_FLOOR_PCT);
  assert.equal(memory.max, 30 * MIB);
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
