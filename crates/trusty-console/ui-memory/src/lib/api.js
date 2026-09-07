/*
 * Why: All UI components hit the same trusty-memory daemon REST surface;
 * centralizing fetch logic gives us one place to handle errors and JSON
 * parsing. #6155: trusty-memory stopped binding a listener (#6286, ADR-0032),
 * so these paths no longer reach a daemon directly. trusty-console serves this
 * SPA at `/tools/memory/` and injects `window.__MEMORY_BASE__ =
 * /api/memory/`, so every path below resolves against that prefix and
 * `crate::memory_uds` translates it into one `memory.*` JSON-RPC call on the
 * daemon's Unix socket. The path SHAPES are unchanged, because that mapping
 * table is written against them.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx.
 * Test: `crates/trusty-console/tests/memory_uds_bridge.rs` asserts each path
 * below reaches the method it stands for.
 */

import { apiUrl } from './base.js';

async function request(path, opts = {}) {
  const res = await fetch(apiUrl(path), {
    headers: { 'Content-Type': 'application/json', ...(opts.headers || {}) },
    ...opts
  });
  if (!res.ok) {
    let detail = '';
    try {
      detail = await res.text();
    } catch {
      /* ignore */
    }
    throw new Error(`${res.status} ${res.statusText}: ${detail}`);
  }
  if (res.status === 204) return null;
  const ct = res.headers.get('content-type') || '';
  if (ct.includes('application/json')) return res.json();
  return res.text();
}

/**
 * Why: `memory.palaces_list` answers `{"palaces": [{id, palace, error}]}` — a
 * row per palace, because a palace whose counts cannot be read reports why
 * instead of being dropped (see `PalaceListRow` in trusty-memory). Every view
 * here iterates a flat array of palace objects, which is what the retired
 * `GET /api/v1/palaces` route returned, so the wrapper reached
 * `Palaces.svelte` and `KG.svelte` as `TypeError: S is not iterable` and both
 * views stalled. The unwrap belongs here and not in the console bridge, which
 * maps method NAMES and leaves payloads alone.
 * What: one flat palace object per row. A row whose `palace` is missing keeps
 * its id and its `error` and reports `cached: false`, so `countLabel` in
 * `Palaces.svelte` renders `—` (unknown) rather than `0` (empty) — the row
 * stays visible, which is the property the daemon's error field exists to
 * preserve. A bare array passes through, so a daemon predating the wrapper
 * still renders.
 * Test: `src/lib/api.test.js`.
 * @param {unknown} payload The parsed `/api/v1/palaces` body.
 * @returns {Array<object>} Palace objects, flat.
 */
export function unwrapPalaceList(payload) {
  const rows = Array.isArray(payload)
    ? payload
    : Array.isArray(payload?.palaces)
      ? payload.palaces
      : [];
  return rows.flatMap((row) => {
    if (!row || typeof row !== 'object') return [];
    if (row.palace && typeof row.palace === 'object') {
      // `PalaceInfo` carries its own id; fall back to the row's when it does not.
      return [row.palace.id == null ? { ...row.palace, id: row.id } : row.palace];
    }
    if (row.error) return [{ id: row.id, error: row.error, cached: false }];
    return [row];
  });
}

export const api = {
  /** Daemon liveness + resource metrics. */
  health: () => request('/health'),

  /** Aggregate daemon status (palace/drawer/vector/triple counts). */
  status: () => request('/api/v1/status'),

  /** Daemon configuration (provider, model, data root). */
  config: () => request('/api/v1/config'),

  /**
   * List all memory palaces with their metadata + counts, flat.
   * See `unwrapPalaceList` above for why the daemon's wrapper is unwrapped here.
   */
  // #6155: `memory.palaces_list` answers `{palaces: [{id, palace, error}]}`;
  // every view iterates palace objects.
  listPalaces: () => request('/api/v1/palaces').then(unwrapPalaceList),

  /** Single palace detail by id. */
  getPalace: (id) => request(`/api/v1/palaces/${encodeURIComponent(id)}`),

  /**
   * List drawers within a palace. Optional `room` narrows to one room;
   * `limit` caps the result count.
   */
  listDrawers: (id, { room, tag, limit } = {}) => {
    const params = new URLSearchParams();
    if (room) params.set('room', room);
    if (tag) params.set('tag', tag);
    if (limit) params.set('limit', String(limit));
    const qs = params.toString();
    return request(
      `/api/v1/palaces/${encodeURIComponent(id)}/drawers${qs ? `?${qs}` : ''}`
    );
  },

  /** Tail the daemon's in-memory log ring buffer. */
  logsTail: (n = 200) =>
    request(`/api/v1/logs/tail?n=${encodeURIComponent(n)}`),

  /** Aggregate dream-cycle stats across all palaces. */
  dreamStatus: () => request('/api/v1/dream/status'),

  /** Trigger a dream cycle across all palaces and return aggregate stats. */
  dreamRun: () => request('/api/v1/dream/run', { method: 'POST' }),

  /** Request a graceful daemon shutdown. */
  stopDaemon: () => request('/api/v1/admin/stop', { method: 'POST' }),

  /**
   * List distinct active subjects paired with their active-triple count.
   * Why: KG Explorer renders a count badge next to each subject and
   * supports sort-by-count without N round-trips.
   */
  kgListSubjectsWithCounts: (id, limit = 200) =>
    request(
      `/api/v1/palaces/${encodeURIComponent(id)}/kg/subjects_with_counts?limit=${encodeURIComponent(limit)}`
    ),

  /**
   * List active triples in a palace's KG, paginated by `valid_from DESC`.
   * Why: KG Explorer "All" mode — table view without a subject filter.
   */
  kgListAll: (id, { limit = 50, offset = 0 } = {}) =>
    request(
      `/api/v1/palaces/${encodeURIComponent(id)}/kg/all?limit=${encodeURIComponent(limit)}&offset=${encodeURIComponent(offset)}`
    ),

  /**
   * Query triples by subject within a single palace.
   * Why: KG Explorer right panel when a subject is selected.
   */
  // #6155: the console bridges this path onto trusty-memory's `kg_query` tool,
  // which answers `{subject, triples, kg_triple_count}` rather than the bare
  // array the retired HTTP route returned. Callers still want the array, so
  // unwrap it here rather than in the bridge — the bridge maps names, not
  // payload shapes.
  kgQuery: (id, subject) =>
    request(
      `/api/v1/palaces/${encodeURIComponent(id)}/kg?subject=${encodeURIComponent(subject)}`
    ).then((r) => (Array.isArray(r) ? r : (r?.triples ?? []))),

  /** Count of currently-active triples for a palace. */
  kgCount: (id) => request(`/api/v1/palaces/${encodeURIComponent(id)}/kg/count`),

  /**
   * List entries from the persistent activity log (issue #96), newest
   * first. Used by `ActivityFeed.svelte` to hydrate on mount and to page
   * on scroll.
   * Params:
   *   - limit: page size (1..=500, default 50)
   *   - offset: number of rows to skip
   *   - palace: filter to one palace id
   *   - source: filter to 'http' | 'mcp' | 'hook'
   *   - since / until: ISO-8601 timestamps for time-range filters
   */
  listActivity: ({ limit, offset, palace, source, since, until } = {}) => {
    const params = new URLSearchParams();
    if (limit != null) params.set('limit', String(limit));
    if (offset != null) params.set('offset', String(offset));
    if (palace) params.set('palace', palace);
    if (source) params.set('source', source);
    if (since) params.set('since', since);
    if (until) params.set('until', until);
    const qs = params.toString();
    return request(`/api/v1/activity${qs ? `?${qs}` : ''}`);
  },

  /**
   * Full graph payload for a palace: triples + node/edge/community counts.
   * Why: Issue #97 — one call, every active triple. As of issue #4670 this is
   * the graph view's explicit "load everything" mode, NOT its default: the
   * server caps `triples` at 5,000 and the response now reports
   * `returned_triple_count` / `active_triple_count` / `truncated` so callers
   * cannot mistake a capped payload for the whole graph.
   */
  kgGraph: (id) =>
    request(`/api/v1/palaces/${encodeURIComponent(id)}/kg/graph`),

  /**
   * Top-`limit` nodes by degree plus the edges among them.
   * Why: Issue #4670 — first paint of the graph view. Returns the graph's
   * high-degree skeleton (server default 75, max 200) alongside the
   * palace-wide totals so the header can state "N of M nodes shown".
   */
  kgGraphSeed: (id, limit) => {
    const qs = limit == null ? '' : `?limit=${encodeURIComponent(limit)}`;
    return request(`/api/v1/palaces/${encodeURIComponent(id)}/kg/graph/seed${qs}`);
  },

  /**
   * Bounded expansion around one node, for click-to-expand.
   * Why: Issue #4670 — `direction=in` is the only way to reach a node's
   * incoming edges; `kg?subject=` is a subject prefix scan and never reads
   * the object side.
   * @param direction 'in' | 'out' | 'both' (default 'both')
   * @param maxHops clamped server-side to [1, 4]
   */
  kgNeighbors: (id, node, { direction = 'both', maxHops = 1 } = {}) => {
    const params = new URLSearchParams({
      node,
      direction,
      max_hops: String(maxHops)
    });
    return request(
      `/api/v1/palaces/${encodeURIComponent(id)}/kg/graph/neighbors?${params}`
    );
  }
};
