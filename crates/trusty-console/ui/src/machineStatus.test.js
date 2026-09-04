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
  hostGraphSpec,
  hostGraphs,
  pressureTone,
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

// ── graph specs (#6643) ────────────────────────────────────────────────────

test('hostGraphSpec bands cpu and memory against the server thresholds', () => {
  // The card's badge is classified server-side at 80/95; bars that disagreed
  // with the badge above them would be the contradiction this shares to avoid.
  for (const key of ['cpu', 'memory']) {
    const spec = hostGraphSpec(key, [10, 85, 97]);
    assert.equal(spec.max, 100);
    assert.deepEqual(spec.thresholds, { warning: 80, critical: 95 });
    assert.equal(spec.label, `${key} usage %, one bar per second`);
    assert.deepEqual(spec.values, [10, 85, 97]);
  }
});

test('hostGraphSpec bands disk later than cpu and memory', () => {
  assert.deepEqual(hostGraphSpec('disk', []).thresholds, { warning: 85, critical: 95 });
});

test('hostGraphSpec scales the network card to the busiest second in the window', () => {
  const spec = hostGraphSpec('network', [1_000_000, 4_000_000, null]);
  assert.equal(spec.max, 4_000_000);
  // No band: a busy link is not a fault, so nothing on this card turns amber.
  assert.equal(spec.thresholds, null);
  assert.equal(spec.label, 'Total throughput, one bar per second, peak 4.0 MB/s');
});

test('hostGraphSpec survives an absent series and an unknown card', () => {
  const empty = hostGraphSpec('network', undefined);
  assert.deepEqual(empty.values, []);
  // windowMax's floor of 1 keeps an all-empty network window from dividing by
  // zero.
  assert.equal(empty.max, 1);
  // A card a newer build added draws as a banded level rather than not at all.
  assert.deepEqual(hostGraphSpec('gpu', [50]).thresholds, { warning: 80, critical: 95 });
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

// ── #6642: the per-card history series ─────────────────────────────────────

test('hostGraphs projects one series per card, oldest first', () => {
  const samples = [
    {
      cpu: { usage_pct: 10 },
      memory: { usage_pct: 20 },
      disks: { aggregate_usage_pct: 30 },
      network: { rx_bytes_per_sec: 100, tx_bytes_per_sec: 25 },
    },
    {
      cpu: { usage_pct: 11 },
      memory: { usage_pct: 21 },
      disks: { aggregate_usage_pct: 31 },
      network: { rx_bytes_per_sec: 200, tx_bytes_per_sec: 50 },
    },
  ];
  const graphs = hostGraphs(samples);
  assert.deepEqual(graphs.cpu, [10, 11]);
  assert.deepEqual(graphs.memory, [20, 21]);
  assert.deepEqual(graphs.disk, [30, 31]);
  assert.deepEqual(graphs.network, [125, 250], 'network is rx + tx bytes/sec');
  assert.deepEqual(Object.keys(graphs).sort(), ['cpu', 'disk', 'memory', 'network']);
});

test('a sample missing a subsystem yields a gap, not a zero', () => {
  const graphs = hostGraphs([{ cpu: { usage_pct: 5 } }]);
  assert.deepEqual(graphs.cpu, [5]);
  assert.deepEqual(graphs.memory, [null]);
  assert.deepEqual(graphs.network, [null]);
});

test('hostGraphs on an empty or absent ring yields empty series', () => {
  assert.deepEqual(hostGraphs([]).cpu, []);
  assert.deepEqual(hostGraphs(undefined).network, []);
});
