/**
 * The machine-status dashboard's data layer (#6518).
 *
 * Why: everything the Overview dashboard shows is a projection of one JSON
 * payload — `GET /api/console/machine-status`, the phase-1 `MachineStatus`
 * (crates/trusty-common/src/console_metrics/machine_status.rs). Keeping the
 * fetch, the 503 branch, the unit conversions and the tone mapping here leaves
 * `MachineStatusPanel.svelte` a pure renderer, and lets every one of those
 * decisions be tested with `node --test` rather than a browser.
 *
 * Two rules this module exists to hold:
 *   - No client-side thresholds. `pressure` and the per-service `status` are
 *     classified server-side against `HostThresholds`; this file only maps the
 *     server's answer onto a Foundry badge tone. A percentage is never compared
 *     to a number here.
 *   - An unrecognised enum variant is neutral, never an alarm. `Pressure` and
 *     `ServiceHealth` are `#[non_exhaustive]` on the Rust side, so a newer
 *     daemon can ship a variant this bundle predates — the committed SPA
 *     bundle makes that likely, not hypothetical. Unknown renders muted with
 *     its own label rather than falling into "critical".
 *
 * What: pure functions over the payload plus one fetch wrapper that never
 * rejects. Byte fields arrive as raw bytes and rates as bytes/sec; GiB is
 * binary (1024³) and MB/s is decimal (10⁶), each matching the unit it prints.
 * Test: `machineStatus.test.js` — run `node --test src/machineStatus.test.js`
 * from `crates/trusty-console/ui`.
 */

import { formatLastUsed } from './lastUsed.js';

/** The phase-1 route this dashboard renders. */
export const MACHINE_STATUS_URL = '/api/console/machine-status';

/**
 * What the dashboard says while the host cache is still cold.
 *
 * The route answers 503 until the background sampler completes its first pass,
 * which is a normal few seconds after boot — not a fault. The wording says so,
 * because "HTTP 503" reads as a broken daemon.
 */
export const COLD_CACHE_MESSAGE =
  'Machine status not yet available — first sample pending';

/** The placeholder every formatter returns for an absent or unreadable value. */
export const UNKNOWN = '—';

const BYTES_PER_GIB = 1024 * 1024 * 1024;
const BYTES_PER_MB = 1000 * 1000;

/** True only for a real, finite number — `null`, `undefined` and NaN are not. */
function isNum(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

/** A byte count as a bare 1-decimal GiB figure, with no unit suffix. */
function gib(bytes) {
  return isNum(bytes) ? (bytes / BYTES_PER_GIB).toFixed(1) : null;
}

/** A byte count as `"12.3 GiB"`, or the placeholder. */
export function formatBytesGiB(bytes) {
  const value = gib(bytes);
  return value === null ? UNKNOWN : `${value} GiB`;
}

/**
 * A used/total byte pair as `"12.3 / 32.0 GiB"` — one unit, stated once.
 *
 * Either half being unreadable collapses the whole pair to the placeholder:
 * half a ratio is worse than none, because it reads as a real measurement.
 */
export function formatGiBPair(used, total) {
  const u = gib(used);
  const t = gib(total);
  return u === null || t === null ? UNKNOWN : `${u} / ${t} GiB`;
}

/** A byte rate as `"2.4 MB/s"`, or the placeholder. */
export function formatRateMBs(bytesPerSec) {
  return isNum(bytesPerSec)
    ? `${(bytesPerSec / BYTES_PER_MB).toFixed(1)} MB/s`
    : UNKNOWN;
}

/** A percentage as `"41.2%"`, or the placeholder. */
export function formatPct(pct) {
  return isNum(pct) ? `${pct.toFixed(1)}%` : UNKNOWN;
}

/** Both directions of throughput on one line: `"↓ 2.4 MB/s ↑ 0.3 MB/s"`. */
export function formatNetworkRates(network) {
  return `↓ ${formatRateMBs(network?.rx_bytes_per_sec)} ↑ ${formatRateMBs(
    network?.tx_bytes_per_sec,
  )}`;
}

/**
 * The Foundry badge tone for a host subsystem's `Pressure`.
 *
 * The server has already classified; this is a lookup, not a judgement. An
 * unrecognised variant is `muted` — see the module header.
 */
export function pressureTone(pressure) {
  switch (pressure) {
    case 'nominal':
      return 'success';
    case 'warning':
      return 'warning';
    case 'critical':
      return 'danger';
    default:
      return 'muted';
  }
}

/** The Foundry badge tone for one service's `ServiceHealth`. */
export function serviceHealthTone(health) {
  switch (health) {
    case 'ok':
      return 'success';
    case 'degraded':
      return 'warning';
    case 'error':
      return 'danger';
    default:
      return 'muted';
  }
}

/**
 * The rollup card's tone: the worst health any service reports.
 *
 * Mirrors `Pressure::worst` on the host side — one error outranks any number of
 * degraded, so the card never reads healthier than its worst row.
 */
export function rollupTone(rollup) {
  if (!rollup) return 'muted';
  if (rollup.error > 0) return 'danger';
  if (rollup.degraded > 0) return 'warning';
  return 'success';
}

/** A badge's text for an enum value: its own name, or `UNKNOWN` when absent. */
export function toneLabel(value) {
  return typeof value === 'string' && value.trim() !== ''
    ? value.toUpperCase()
    : 'UNKNOWN';
}

/**
 * The CPU card's meta line: `"10 logical · 8 physical"`.
 *
 * `physical_cores` is `None` on OSes that hide the split, so that half is
 * dropped rather than printed as a dash inside a sentence.
 */
export function cpuMeta(cpu) {
  if (!isNum(cpu?.logical_cores)) return '';
  const parts = [`${cpu.logical_cores} logical`];
  if (isNum(cpu.physical_cores)) parts.push(`${cpu.physical_cores} physical`);
  return parts.join(' · ');
}

/**
 * The memory card's swap line, or `null` when the host has no swap.
 *
 * A zero swap total is a configuration, not a measurement — printing
 * "0.0 / 0.0 GiB" invites the reader to wonder what broke.
 */
export function swapLine(memory) {
  if (!isNum(memory?.swap_total_bytes) || memory.swap_total_bytes <= 0) {
    return null;
  }
  return `Swap ${formatGiBPair(memory.swap_used_bytes, memory.swap_total_bytes)}`;
}

/**
 * The four top-row stat cards, in display order.
 *
 * Each row is `{ key, label, value, meta, tone, badge, extra }`. `tone` is
 * `null` where the payload carries no pressure signal (network is a rate, not
 * a level), and the whole set degrades to placeholders when `host` is absent —
 * which is what the 503 state renders, so the grid keeps its shape instead of
 * collapsing to an error box.
 */
export function statCards(host) {
  const cpu = host?.cpu;
  const memory = host?.memory;
  const disks = host?.disks;

  return [
    {
      key: 'cpu',
      label: 'CPU',
      value: formatPct(cpu?.usage_pct),
      meta: cpuMeta(cpu),
      tone: cpu ? pressureTone(cpu.pressure) : null,
      badge: cpu ? toneLabel(cpu.pressure) : null,
      extra: null,
    },
    {
      key: 'memory',
      label: 'Memory',
      value: formatGiBPair(memory?.used_bytes, memory?.total_bytes),
      meta: memory ? `${formatPct(memory.usage_pct)} used` : '',
      tone: memory ? pressureTone(memory.pressure) : null,
      badge: memory ? toneLabel(memory.pressure) : null,
      extra: swapLine(memory),
    },
    {
      key: 'disk',
      label: 'Disk',
      value: formatGiBPair(disks?.aggregate_used_bytes, disks?.aggregate_total_bytes),
      meta: disks ? `${formatPct(disks.aggregate_usage_pct)} used` : '',
      tone: disks ? pressureTone(disks.pressure) : null,
      badge: disks ? toneLabel(disks.pressure) : null,
      extra: null,
    },
    {
      key: 'network',
      label: 'Network',
      value: host?.network ? formatNetworkRates(host.network) : UNKNOWN,
      meta: isNum(host?.network?.window_secs)
        ? `over ${host.network.window_secs.toFixed(1)}s`
        : '',
      // Throughput has no server-side pressure band — a busy link is not a
      // fault — so this card carries no tone and no badge.
      tone: null,
      badge: null,
      extra: null,
    },
  ];
}

/**
 * One table row per service in the rollup, in the order the server sent.
 *
 * `collected_at_unix` is rendered through `formatLastUsed` rather than a second
 * relative-time implementation: the buckets ("just now", "4m ago", a date past
 * a month) are the ones the Search and Memory rosters already use, and a
 * console showing two different relative-time vocabularies is a defect. The
 * shim is the field rename — that function reads `last_used_unix`.
 */
export function serviceRows(status, now = Math.floor(Date.now() / 1000)) {
  const services = status?.services?.services ?? [];
  return services.map((s) => ({
    id: s.service_id,
    displayName: s.display_name || s.service_id || UNKNOWN,
    version: s.version || UNKNOWN,
    health: s.status,
    tone: serviceHealthTone(s.status),
    healthLabel: toneLabel(s.status),
    collected: formatLastUsed({ last_used_unix: s.collected_at_unix }, now),
  }));
}

/**
 * Fetch the machine status, resolving to a `{ status, error, cold }` triple.
 *
 * Never rejects: the panel polls on an interval and a thrown error inside a
 * timer callback is unhandled. The 503 test comes BEFORE the generic `!ok`
 * throw so a cold cache reports [`COLD_CACHE_MESSAGE`] rather than "HTTP 503"
 * — the same ordering `SearchTab.svelte` uses for the per-service routes.
 * `fetchImpl` is a parameter so every branch is testable without a network.
 */
export async function fetchMachineStatus(fetchImpl = fetch) {
  try {
    const resp = await fetchImpl(MACHINE_STATUS_URL);
    if (resp.status === 503) {
      return { status: null, error: COLD_CACHE_MESSAGE, cold: true };
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return { status: await resp.json(), error: null, cold: false };
  } catch (e) {
    return { status: null, error: e.message, cold: false };
  }
}

/**
 * The four host series the stat-card bar graphs draw (#6642).
 *
 * Why here: a series is one more projection of the same payload the cards above
 * render, and deriving it beside `statCards` is what keeps the headline number
 * and the newest bar reading the same field. The keys match `statCards`' `key`
 * so the panel can look a series up by card without a second mapping.
 *
 * What: `samples` is the host ring from the history snapshot, oldest first.
 * CPU, memory and disk are percentages and share the graph's 0–100 scale.
 * Network has no percentage, so its series is TOTAL throughput —
 * `rx + tx` bytes/sec — which the card then scales to the largest value in the
 * window; the absolute figure stays on the card's value line. A sample missing
 * a subsystem yields `null` at that slot, which the graph draws as a gap.
 * Test: `hostGraphs projects one series per card, oldest first` and the
 * null/empty cases beside it in `machineStatus.test.js`.
 */
export function hostGraphs(samples) {
  const list = Array.isArray(samples) ? samples : [];
  const at = (read) => list.map((s) => (isNum(read(s)) ? read(s) : null));
  return {
    cpu: at((s) => s?.cpu?.usage_pct),
    memory: at((s) => s?.memory?.usage_pct),
    disk: at((s) => s?.disks?.aggregate_usage_pct),
    network: at((s) => totalRate(s?.network)),
  };
}

/** Both directions summed, or `null` when neither is a readable number. */
function totalRate(network) {
  const rx = isNum(network?.rx_bytes_per_sec) ? network.rx_bytes_per_sec : null;
  const tx = isNum(network?.tx_bytes_per_sec) ? network.tx_bytes_per_sec : null;
  if (rx === null && tx === null) return null;
  return (rx ?? 0) + (tx ?? 0);
}
