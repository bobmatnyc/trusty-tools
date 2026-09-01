/**
 * Tests for the machine-status dashboard's data layer (#6518).
 *
 * Run: `node --test src/machineStatus.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  COLD_CACHE_MESSAGE,
  MACHINE_STATUS_URL,
  UNKNOWN,
  cpuMeta,
  fetchMachineStatus,
  formatBytesGiB,
  formatGiBPair,
  formatNetworkRates,
  formatPct,
  formatRateMBs,
  pressureTone,
  rollupTone,
  serviceHealthTone,
  serviceRows,
  statCards,
  swapLine,
  toneLabel,
} from './machineStatus.js';

const GIB = 1024 * 1024 * 1024;

/**
 * A `MachineStatus` fixture shaped exactly as
 * crates/trusty-common/src/console_metrics/machine_status.rs serialises it.
 */
function machineStatusFixture() {
  return {
    host: {
      cpu: {
        usage_pct: 41.23,
        logical_cores: 10,
        physical_cores: 8,
        pressure: 'nominal',
      },
      memory: {
        total_bytes: 32 * GIB,
        used_bytes: 12.3 * GIB,
        available_bytes: 19.7 * GIB,
        usage_pct: 38.44,
        swap_total_bytes: 4 * GIB,
        swap_used_bytes: 1.2 * GIB,
        pressure: 'nominal',
      },
      disks: {
        aggregate_total_bytes: 926.5 * GIB,
        aggregate_available_bytes: 514 * GIB,
        aggregate_used_bytes: 412.5 * GIB,
        aggregate_usage_pct: 44.53,
        pressure: 'nominal',
        mounts: [],
      },
      network: {
        rx_bytes_per_sec: 2_400_000,
        tx_bytes_per_sec: 300_000,
        rx_total_bytes: 91_000_000,
        tx_total_bytes: 12_000_000,
        window_secs: 15.02,
      },
      overall_pressure: 'nominal',
      sampled_at_unix: 1_800_000_000,
    },
    services: {
      total: 3,
      ok: 2,
      degraded: 1,
      error: 0,
      services: [
        {
          service_id: 'trusty-search',
          display_name: 'Trusty Search',
          version: '0.24.1',
          status: 'ok',
          metrics_schema_version: 3,
          collected_at_unix: 1_800_000_000 - 120,
        },
        {
          service_id: 'trusty-memory',
          display_name: 'Trusty Memory',
          version: '0.46.5',
          status: 'degraded',
          metrics_schema_version: 1,
          collected_at_unix: 1_800_000_000 - 7200,
        },
        {
          service_id: 'trusty-mpm',
          display_name: 'Trusty MPM',
          version: '1.4.0',
          status: 'ok',
          metrics_schema_version: 1,
          collected_at_unix: null,
        },
      ],
    },
    schema_version: 1,
    assembled_at_unix: 1_800_000_000,
  };
}

// ── unit conversion ────────────────────────────────────────────────────────

test('formatBytesGiB renders zero, the 1 GiB boundary, and a large host', () => {
  assert.equal(formatBytesGiB(0), '0.0 GiB');
  assert.equal(formatBytesGiB(GIB), '1.0 GiB');
  assert.equal(formatBytesGiB(GIB - 1), '1.0 GiB'); // rounds, never truncates to 0.9
  assert.equal(formatBytesGiB(64 * GIB), '64.0 GiB');
  assert.equal(formatBytesGiB(8 * 1024 * GIB), '8192.0 GiB');
});

test('an absent or unreadable byte count is the placeholder, not zero', () => {
  // Reporting "0.0 GiB" for a field the daemon did not send claims a
  // measurement that was never taken.
  assert.equal(formatBytesGiB(undefined), UNKNOWN);
  assert.equal(formatBytesGiB(null), UNKNOWN);
  assert.equal(formatBytesGiB(NaN), UNKNOWN);
  assert.equal(formatBytesGiB('12'), UNKNOWN);
});

test('formatGiBPair states the unit once, and collapses on a half-reading', () => {
  assert.equal(formatGiBPair(12.3 * GIB, 32 * GIB), '12.3 / 32.0 GiB');
  assert.equal(formatGiBPair(0, 32 * GIB), '0.0 / 32.0 GiB');
  assert.equal(formatGiBPair(12.3 * GIB, null), UNKNOWN);
  assert.equal(formatGiBPair(null, 32 * GIB), UNKNOWN);
});

test('formatRateMBs renders an idle link and a sub-megabyte one', () => {
  assert.equal(formatRateMBs(0), '0.0 MB/s');
  assert.equal(formatRateMBs(300_000), '0.3 MB/s');
  assert.equal(formatRateMBs(2_400_000), '2.4 MB/s');
  assert.equal(formatRateMBs(1_250_000_000), '1250.0 MB/s');
  assert.equal(formatRateMBs(undefined), UNKNOWN);
});

test('formatPct and formatNetworkRates', () => {
  assert.equal(formatPct(41.23), '41.2%');
  assert.equal(formatPct(0), '0.0%');
  assert.equal(formatPct(null), UNKNOWN);
  assert.equal(
    formatNetworkRates({ rx_bytes_per_sec: 2_400_000, tx_bytes_per_sec: 300_000 }),
    '↓ 2.4 MB/s ↑ 0.3 MB/s',
  );
});

// ── tone mapping ───────────────────────────────────────────────────────────

test('pressureTone maps the three server-side bands', () => {
  assert.equal(pressureTone('nominal'), 'success');
  assert.equal(pressureTone('warning'), 'warning');
  assert.equal(pressureTone('critical'), 'danger');
});

test('an unrecognised pressure is muted, never an alarm', () => {
  // `Pressure` is #[non_exhaustive] and the SPA bundle is committed, so a newer
  // daemon can ship a variant this bundle predates. Guessing "critical" would
  // page someone over a rename.
  assert.equal(pressureTone('saturated'), 'muted');
  assert.equal(pressureTone(undefined), 'muted');
  assert.equal(pressureTone(null), 'muted');
  assert.equal(pressureTone('NOMINAL'), 'muted'); // the wire form is lowercase
});

test('serviceHealthTone maps ok/degraded/error and falls back to muted', () => {
  assert.equal(serviceHealthTone('ok'), 'success');
  assert.equal(serviceHealthTone('degraded'), 'warning');
  assert.equal(serviceHealthTone('error'), 'danger');
  assert.equal(serviceHealthTone('starting'), 'muted');
  assert.equal(serviceHealthTone(undefined), 'muted');
});

test('rollupTone reports the worst health any service holds', () => {
  assert.equal(rollupTone({ total: 3, ok: 3, degraded: 0, error: 0 }), 'success');
  assert.equal(rollupTone({ total: 3, ok: 2, degraded: 1, error: 0 }), 'warning');
  assert.equal(rollupTone({ total: 3, ok: 1, degraded: 1, error: 1 }), 'danger');
  // One error outranks any number of degraded.
  assert.equal(rollupTone({ total: 9, ok: 0, degraded: 8, error: 1 }), 'danger');
  assert.equal(rollupTone(null), 'muted');
});

test('toneLabel names the value it stamps', () => {
  assert.equal(toneLabel('nominal'), 'NOMINAL');
  assert.equal(toneLabel('degraded'), 'DEGRADED');
  assert.equal(toneLabel(''), 'UNKNOWN');
  assert.equal(toneLabel(undefined), 'UNKNOWN');
});

// ── card mapping ───────────────────────────────────────────────────────────

test('cpuMeta drops the physical count the OS did not report', () => {
  assert.equal(cpuMeta({ logical_cores: 10, physical_cores: 8 }), '10 logical · 8 physical');
  assert.equal(cpuMeta({ logical_cores: 4, physical_cores: null }), '4 logical');
  assert.equal(cpuMeta(undefined), '');
});

test('swapLine appears only on a host that has swap', () => {
  assert.equal(
    swapLine({ swap_total_bytes: 4 * GIB, swap_used_bytes: 1.2 * GIB }),
    'Swap 1.2 / 4.0 GiB',
  );
  assert.equal(swapLine({ swap_total_bytes: 0, swap_used_bytes: 0 }), null);
  assert.equal(swapLine(undefined), null);
});

test('statCards maps a real host onto the four Foundry stat cards', () => {
  const cards = statCards(machineStatusFixture().host);
  assert.deepEqual(
    cards.map((c) => c.key),
    ['cpu', 'memory', 'disk', 'network'],
  );

  const [cpu, memory, disk, network] = cards;
  assert.equal(cpu.value, '41.2%');
  assert.equal(cpu.meta, '10 logical · 8 physical');
  assert.equal(cpu.tone, 'success');
  assert.equal(cpu.badge, 'NOMINAL');

  assert.equal(memory.value, '12.3 / 32.0 GiB');
  assert.equal(memory.meta, '38.4% used');
  assert.equal(memory.extra, 'Swap 1.2 / 4.0 GiB');

  assert.equal(disk.value, '412.5 / 926.5 GiB');
  assert.equal(disk.meta, '44.5% used');

  assert.equal(network.value, '↓ 2.4 MB/s ↑ 0.3 MB/s');
  assert.equal(network.meta, 'over 15.0s');
  // Throughput carries no server-side band, so the card must not stamp one.
  assert.equal(network.tone, null);
  assert.equal(network.badge, null);
});

test('with no host the grid keeps its four cards and shows placeholders', () => {
  // This is the 503 render: the dashboard must not collapse to an error box,
  // because the layout is what tells the operator the sample is merely pending.
  const cards = statCards(null);
  assert.equal(cards.length, 4);
  for (const card of cards) {
    assert.equal(card.value, UNKNOWN);
    assert.equal(card.tone, null);
    assert.equal(card.badge, null);
  }
});

// ── rollup rows ────────────────────────────────────────────────────────────

test('serviceRows maps the rollup to table rows in the order sent', () => {
  const now = 1_800_000_000;
  const rows = serviceRows(machineStatusFixture(), now);

  assert.equal(rows.length, 3);
  assert.deepEqual(
    rows.map((r) => r.displayName),
    ['Trusty Search', 'Trusty Memory', 'Trusty MPM'],
  );
  assert.deepEqual(
    rows.map((r) => r.tone),
    ['success', 'warning', 'success'],
  );
  assert.deepEqual(
    rows.map((r) => r.healthLabel),
    ['OK', 'DEGRADED', 'OK'],
  );
  assert.equal(rows[0].version, '0.24.1');
  // Relative time comes from lastUsed.js, so the buckets match the Search and
  // Memory rosters rather than inventing a second vocabulary.
  assert.equal(rows[0].collected, '2m ago');
  assert.equal(rows[1].collected, '2h ago');
  // A service that reported no collection time shows the same dash a
  // never-used index does, not the unix epoch.
  assert.equal(rows[2].collected, '—');
});

test('serviceRows is empty, not a throw, when nothing has reported', () => {
  assert.deepEqual(serviceRows(null), []);
  assert.deepEqual(serviceRows({ services: { total: 0, ok: 0, degraded: 0, error: 0, services: [] } }), []);
});

// ── fetch branching ────────────────────────────────────────────────────────

test('a cold host cache reports the pending-sample message, not HTTP 503', () => {
  // The 503 branch must be tested BEFORE the generic !ok throw, or a normal
  // few-seconds-after-boot state reads as a broken daemon.
  return fetchMachineStatus(async () => ({ status: 503, ok: false })).then((r) => {
    assert.equal(r.cold, true);
    assert.equal(r.error, COLD_CACHE_MESSAGE);
    assert.equal(r.status, null);
  });
});

test('a warm cache resolves the parsed MachineStatus', async () => {
  const payload = machineStatusFixture();
  const seen = [];
  const result = await fetchMachineStatus(async (url) => {
    seen.push(url);
    return { status: 200, ok: true, json: async () => payload };
  });
  assert.deepEqual(seen, [MACHINE_STATUS_URL]);
  assert.equal(result.cold, false);
  assert.equal(result.error, null);
  assert.equal(result.status.services.total, 3);
});

test('every other failure resolves rather than rejecting', async () => {
  // The panel polls on a timer; a rejection inside that callback is unhandled.
  const notFound = await fetchMachineStatus(async () => ({ status: 404, ok: false }));
  assert.equal(notFound.cold, false);
  assert.equal(notFound.error, 'HTTP 404');

  const refused = await fetchMachineStatus(async () => {
    throw new Error('connection refused');
  });
  assert.equal(refused.error, 'connection refused');
  assert.equal(refused.status, null);

  const unparseable = await fetchMachineStatus(async () => ({
    status: 200,
    ok: true,
    json: async () => {
      throw new SyntaxError('not JSON');
    },
  }));
  assert.equal(unparseable.status, null);
  assert.equal(unparseable.error, 'not JSON');
});
