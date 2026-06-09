/*
 * Why: All UI components hit the same daemon REST surface; centralizing fetch
 * logic gives us one place to handle errors, base URL, and JSON parsing.
 * The daemon serves the bundle at /ui and the API at flat paths
 * (/health, /indexes, /search, ...) so requests are always same-origin in
 * production. In `vite dev`, vite.config.js proxies the API paths through to
 * 127.0.0.1:7878. When served through the trusty-console reverse-proxy at
 * /proxy/search/, apiUrl() rebases absolute paths to the proxy sub-path so
 * every API call reaches the daemon via the proxy instead of 404ing at the
 * console host root.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx.
 *   Non-2xx responses throw an ApiError with a numeric `status` field so
 *   callers can check `e.status === 503` rather than substring-matching the
 *   message string (issue #781).
 * Test: Console-call api.health() and confirm shape matches /health.
 *   For error handling: mock a 503 response and assert e.status === 503.
 *   Proxy mode: open the SPA at /proxy/search/ and confirm api.health()
 *   fetches /proxy/search/health not /health.
 */

import { apiUrl } from './base.js';

/**
 * Why: Callers need a structured way to inspect HTTP errors without
 * substring-matching the message string (issue #781).
 * What: Extends Error with a numeric `status` field (the HTTP status code).
 * Test: Caught errors from api.* calls expose `.status` for reliable comparisons.
 */
export class ApiError extends Error {
  /**
   * @param {number} status  HTTP status code
   * @param {string} message Human-readable description
   */
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

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
    throw new ApiError(res.status, `${res.status} ${res.statusText}: ${detail}`);
  }
  if (res.status === 204) return null;
  const ct = res.headers.get('content-type') || '';
  if (ct.includes('application/json')) return res.json();
  return res.text();
}

export const api = {
  health: () => request('/health'),

  listIndexes: () => request('/indexes'),
  createIndex: (id, root_path) =>
    request('/indexes', {
      method: 'POST',
      body: JSON.stringify({ id, root_path })
    }),
  deleteIndex: (id) =>
    request(`/indexes/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  indexStatus: (id) => request(`/indexes/${encodeURIComponent(id)}/status`),

  /** Per-index hybrid search. */
  search: (id, text, top_k = 10) =>
    request(`/indexes/${encodeURIComponent(id)}/search`, {
      method: 'POST',
      body: JSON.stringify({ text, top_k })
    }),

  /** Cross-collection fan-out search across every registered index. */
  globalSearch: (query, top_k = 10, full_content = false) =>
    request('/search', {
      method: 'POST',
      body: JSON.stringify({ query, top_k, full_content })
    }),

  reindex: (id, root_path) =>
    request(`/indexes/${encodeURIComponent(id)}/reindex`, {
      method: 'POST',
      body: JSON.stringify(root_path ? { root_path } : {})
    }),

  chat: (index_id, message, history = []) =>
    request('/chat', {
      method: 'POST',
      body: JSON.stringify({ index_id, message, history })
    }),

  /** Tail the daemon's in-memory log ring buffer. */
  logsTail: (n = 200) => request(`/logs/tail?n=${encodeURIComponent(n)}`),

  /** Current daemon memory-limit configuration. */
  getConfig: () => request('/config'),

  /**
   * Update daemon runtime config (memory limits). The daemon exposes a
   * PATCH endpoint; omitted fields are left unchanged.
   */
  updateConfig: (patch) =>
    request('/config', {
      method: 'PATCH',
      body: JSON.stringify(patch)
    }),

  /** Request a graceful daemon shutdown. */
  stopDaemon: () => request('/admin/stop', { method: 'POST' })
};
