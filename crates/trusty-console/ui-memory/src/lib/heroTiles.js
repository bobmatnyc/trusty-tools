/**
 * The dashboard's hero row (#6928).
 *
 * Why: the owner's ruling, made while looking at the row reading
 * `94/94 Palaces · 5176 Total Drawers · 5018 Total Vectors · 131 Total Rooms ·
 * 60375 KG Triples`, adds two aggregates to it — the daemon's physical
 * footprint and the palace store's on-disk size — "both aggregate figures in
 * the hero row, not per-palace columns". Building the row as data rather than
 * as markup is what lets a test assert there are exactly seven of them, which
 * a rendered `{#each}` over an inline literal could not.
 *
 * What: one pure function over `report.metrics` from
 * `GET /api/console/metrics/memory` (schema 5). No fetch, no DOM.
 * Test: `heroTiles.test.js`.
 */

/**
 * How many tiles the row carries. A tile added or dropped without the ruling
 * changing is a regression, and `heroTiles.test.js` fails on it.
 */
export const HERO_TILE_COUNT = 7;

/** The tile ids, in the order the row renders them. */
export const HERO_TILE_IDS = [
  'palaces',
  'drawers',
  'vectors',
  'rooms',
  'kg-triples',
  'ram',
  'disk',
];

/** What a tile shows when its figure could not be read. */
export const UNKNOWN = '—';

/**
 * Human-readable byte size: `B` under a kibibyte, then KB / MB / GB.
 *
 * A non-number renders as [`UNKNOWN`] rather than `0 B` — an operator reading
 * `0 B` about a 1.5 GB store would be reading a bug as a measurement.
 *
 * @param {unknown} bytes byte count, or a non-number for "unknown"
 * @returns {string}
 */
export function formatBytes(bytes) {
  if (typeof bytes !== 'number' || !Number.isFinite(bytes) || bytes < 0) return UNKNOWN;
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

/** A count tile's value: the number itself, or the unknown marker. */
function count(value) {
  return typeof value === 'number' ? value.toLocaleString() : UNKNOWN;
}

/**
 * The seven hero tiles for one metrics payload.
 *
 * `unit` is `'bytes'` on exactly the two tiles the ruling requires to carry
 * byte units, so a test can assert that without parsing the rendered string.
 * The Palaces tile keeps the counted/total form the console tab established
 * (#6372): on a host with 94 palaces, "how many were countable" is a different
 * fact from "how many exist", and collapsing them hid 92 uncounted palaces.
 *
 * @param {object|null|undefined} metrics `report.metrics`, schema 5 or older
 * @returns {Array<{id: string, label: string, value: string, sub: string|null, unit: 'count'|'bytes'}>}
 */
export function heroTiles(metrics) {
  const m = metrics ?? {};
  const counted = m.counted_palace_count ?? m.cached_palace_count;
  return [
    {
      id: 'palaces',
      label: 'Palaces',
      value: count(counted),
      sub: typeof m.palace_count === 'number' ? `of ${m.palace_count} on disk` : null,
      unit: 'count',
    },
    { id: 'drawers', label: 'Total Drawers', value: count(m.total_drawers), sub: null, unit: 'count' },
    { id: 'vectors', label: 'Total Vectors', value: count(m.total_vectors), sub: null, unit: 'count' },
    { id: 'rooms', label: 'Total Rooms', value: count(m.total_rooms), sub: null, unit: 'count' },
    {
      id: 'kg-triples',
      label: 'KG Triples',
      value: count(m.total_kg_triples),
      sub: null,
      unit: 'count',
    },
    {
      id: 'ram',
      label: 'RAM',
      value: formatBytes(m.ram_bytes),
      sub: 'physical footprint',
      unit: 'bytes',
    },
    {
      id: 'disk',
      label: 'Disk',
      value: formatBytes(m.disk_bytes),
      sub: 'palace store on disk',
      unit: 'bytes',
    },
  ];
}
