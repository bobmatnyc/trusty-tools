/**
 * Tests for the home page's Services list data layer (#6642).
 *
 * Run: `node --test src/servicesList.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  DASH,
  cpuSeries,
  formatCpu,
  latestSample,
  serviceRows,
  sortByDisplayName,
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

test('an empty roster yields no rows rather than throwing', () => {
  assert.deepEqual(serviceRows(undefined), []);
  assert.deepEqual(serviceRows([]), []);
});
