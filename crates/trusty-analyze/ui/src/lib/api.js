/*
 * Why: All UI components hit the same analyzer REST surface; centralizing fetch
 * logic gives us one place to handle errors, base URL, and JSON parsing.
 * The analyzer serves the bundle at /ui and the API at flat paths
 * (/health, /indexes, /facts, ...) so requests are always same-origin in
 * production. In `vite dev`, vite.config.js proxies the API paths through to
 * 127.0.0.1:7879. When served through the trusty-console reverse-proxy at
 * /proxy/analyze/, apiUrl() rebases absolute paths to the proxy sub-path so
 * every API call reaches the daemon via the proxy instead of 404ing at the
 * console host root.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx.
 * Test: Console-call api.health() and confirm shape matches /health.
 *   Proxy mode: open the SPA at /proxy/analyze/ and confirm api.health()
 *   fetches /proxy/analyze/health not /health.
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

export const api = {
  health: () => request('/health'),
  indexes: () => request('/indexes'),
  complexityHotspots: (id, topK = 20) =>
    request(`/indexes/${encodeURIComponent(id)}/complexity_hotspots?top_k=${topK}`),
  smells: (id, category) =>
    request(
      `/indexes/${encodeURIComponent(id)}/smells${category ? '?category=' + encodeURIComponent(category) : ''}`
    ),
  quality: (id) => request(`/indexes/${encodeURIComponent(id)}/quality`),
  refactorSuggestions: (id, { minSeverity = 'low', topK = 20 } = {}) => {
    const qs = new URLSearchParams({
      min_severity: minSeverity,
      top_k: String(topK)
    });
    return request(`/indexes/${encodeURIComponent(id)}/refactor-suggestions?${qs}`);
  },
  clusters: (id, { k = 8, method = 'bow' } = {}) =>
    request(`/indexes/${encodeURIComponent(id)}/clusters?k=${k}&method=${method}`),
  listFacts: (subject, predicate) => {
    const qs = new URLSearchParams();
    if (subject) qs.set('subject', subject);
    if (predicate) qs.set('predicate', predicate);
    const tail = qs.toString();
    return request(`/facts${tail ? '?' + tail : ''}`);
  },
  upsertFact: (fact) =>
    request('/facts', { method: 'POST', body: JSON.stringify(fact) }),
  deleteFact: (id) =>
    request(`/facts/${encodeURIComponent(id)}`, { method: 'DELETE' })
};
