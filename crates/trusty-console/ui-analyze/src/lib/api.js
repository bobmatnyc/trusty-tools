/*
 * Why: All UI components hit the same analyzer surface; centralizing fetch
 * logic gives us one place to handle errors, base URL, and JSON parsing.
 * #6155: the console serves this SPA at `/tools/analyze/` and answers these
 * paths under `/api/analyze/`, where `analyze_uds` turns each one into a single
 * `analyze.*` JSON-RPC call on the daemon's Unix socket (#6287, ADR-0032).
 * The paths below are unchanged from the retired HTTP router's, so `base.js`
 * reading the injected `window.__ANALYZE_BASE__` is the whole repoint.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx.
 * Test: `crates/trusty-console/tests/analyze_uds_bridge.rs` drives every one of
 * these paths through the real router against a stub daemon socket.
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
