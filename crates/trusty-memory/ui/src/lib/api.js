/*
 * Why: All UI components hit the same trusty-memory daemon REST surface;
 * centralizing fetch logic gives us one place to handle errors and JSON
 * parsing. The daemon serves the SPA at `/` and the API under `/api/v1/*`
 * (plus `/health`), so requests are always same-origin in production. In
 * `vite dev`, vite.config.js proxies `/api` and `/health` to the daemon.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx.
 * Test: Console-call api.health() and confirm the shape matches /health.
 */

async function request(path, opts = {}) {
  const res = await fetch(path, {
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

export const api = {
  /** Daemon liveness + resource metrics. */
  health: () => request('/health'),

  /** Aggregate daemon status (palace/drawer/vector/triple counts). */
  status: () => request('/api/v1/status'),

  /** Daemon configuration (provider, model, data root). */
  config: () => request('/api/v1/config'),

  /** List all memory palaces with their metadata + counts. */
  listPalaces: () => request('/api/v1/palaces'),

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
   * List distinct active subjects in a palace's knowledge graph.
   * Why: KG Explorer left panel — caller doesn't know subjects up front.
   */
  kgListSubjects: (id, limit = 50) =>
    request(
      `/api/v1/palaces/${encodeURIComponent(id)}/kg/subjects?limit=${encodeURIComponent(limit)}`
    ),

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
  kgQuery: (id, subject) =>
    request(
      `/api/v1/palaces/${encodeURIComponent(id)}/kg?subject=${encodeURIComponent(subject)}`
    ),

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
  }
};
