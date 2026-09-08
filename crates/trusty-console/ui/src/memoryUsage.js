/**
 * How the Memory tab reads the daemon's RAM and disk usage (#6928).
 *
 * Why: the tab's whole content is now those two figures, and both have a
 * "could not be measured" state that must not render as zero. RAM has a
 * further wrinkle: the heap / file-backed / compressed split (#7084) is an OS
 * capability, so the payload OMITS those three keys where the OS supplies no
 * such counter rather than nulling them. Deciding all of that in a pure module
 * is what makes it assertable under `node --test`, which cannot mount a Svelte
 * component.
 *
 * What: pure functions over `report.metrics` from
 * `GET /api/console/metrics/memory` (schema 5). No fetch, no DOM.
 * Test: `memoryUsage.test.js` — run `node --test src/memoryUsage.test.js` from
 * `crates/trusty-console/ui`.
 */

import { formatBytes } from './bytes.js';

/**
 * The three ledgers `ram_bytes` divides across, in the order they are shown.
 *
 * Heap first because a growing heap is the one that means a leak; file-backed
 * next because it is the one an mmap-heavy daemon is expected to dominate;
 * compressed last because it is a consequence of the other two.
 */
export const RAM_COMPONENTS = [
  { field: 'ram_heap_bytes', label: 'Heap' },
  { field: 'ram_file_backed_bytes', label: 'File-backed' },
  { field: 'ram_compressed_bytes', label: 'Compressed' },
];

/** What the tab says when the daemon reports a footprint but no split. */
export const NO_BREAKDOWN_NOTE =
  'This daemon reports a total footprint but no heap / file-backed / compressed split.';

/** What the tab says when the daemon reports no footprint at all. */
export const NO_FOOTPRINT_NOTE =
  'This daemon reports no physical footprint — the OS supplies no such counter.';

/**
 * The RAM panel's contents.
 *
 * Why `reported` rather than checking the array's length at the call site: the
 * three components are all-or-nothing — the daemon reads one `task_info` call
 * that either supplies every ledger or none — so a partially-populated split
 * is not a state that exists, and the template should not be written as if it
 * were.
 *
 * @param {object|null|undefined} metrics `report.metrics`, schema 5 or older
 * @returns {{footprintBytes: number|null, footprint: string,
 *            reported: boolean, components: Array<{label: string, bytes: number, text: string}>,
 *            note: string|null}}
 */
export function ramUsage(metrics) {
  const raw = metrics?.ram_bytes;
  const footprintBytes = typeof raw === 'number' ? raw : null;

  const components = RAM_COMPONENTS.filter(
    (c) => typeof metrics?.[c.field] === 'number',
  ).map((c) => ({
    label: c.label,
    bytes: metrics[c.field],
    text: formatBytes(metrics[c.field]),
  }));
  const reported = components.length === RAM_COMPONENTS.length;

  let note = null;
  if (footprintBytes === null) {
    note = NO_FOOTPRINT_NOTE;
  } else if (!reported) {
    note = NO_BREAKDOWN_NOTE;
  }

  return {
    footprintBytes,
    footprint: formatBytes(footprintBytes),
    reported,
    components: reported ? components : [],
    note,
  };
}

/**
 * The disk panel's aggregate figure.
 *
 * `disk_bytes` is always a number from a schema-5 daemon — a walk that reads
 * nothing is a real zero — so a missing value means an older daemon, which
 * renders as unknown rather than as an empty store.
 *
 * @param {object|null|undefined} metrics `report.metrics`
 * @returns {{bytes: number|null, text: string, dataRoot: string|null}}
 */
export function diskUsage(metrics) {
  const raw = metrics?.disk_bytes;
  const bytes = typeof raw === 'number' ? raw : null;
  const dataRoot = typeof metrics?.data_root === 'string' ? metrics.data_root : null;
  return { bytes, text: formatBytes(bytes), dataRoot };
}

/**
 * One palace row's disk cell — the per-palace half of the disk breakdown.
 *
 * @param {object|null|undefined} palace one entry of `metrics.palaces`
 * @returns {string} formatted size, or the unknown marker
 */
export function palaceDiskCell(palace) {
  return formatBytes(palace?.disk_bytes);
}
