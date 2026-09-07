/*
 * Why: All UI components hit the same analyzer surface; centralizing fetch
 * logic gives us one place to handle errors, base URL, and JSON parsing.
 * #6155: the console serves this SPA at `/tools/analyze/` and answers these
 * paths under `/api/analyze/`, where `analyze_uds` turns each one into a single
 * `analyze.*` JSON-RPC call on the daemon's Unix socket (#6287, ADR-0032).
 * The paths below are unchanged from the retired HTTP router's, so `base.js`
 * reading the injected `window.__ANALYZE_BASE__` is the whole repoint.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx. Five of
 * them unwrap a list out of the envelope the daemon answers with; see
 * `unwrapList`.
 * Test: `crates/trusty-console/tests/analyze_uds_bridge.rs` drives every one of
 * these paths through the real router against a stub daemon socket;
 * `src/lib/api.test.js` pins each wrapper's return shape against the daemon's
 * real envelope.
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
 * Why: five `analyze.*` methods answer an OBJECT wrapping their list.
 * `complexity_hotspots` sends `{index_id, top_n, hotspots}`, `smells` a
 * pagination envelope around `chunks`, `refactor_suggestions`
 * `{index_id, count, min_severity, suggestions}`, `clusters` a
 * `ClusterResponse{k, method, dim, iterations, chunk_count, clusters}`, and
 * `facts_list` `{facts, count}`. `state.svelte.js` assigns each result straight
 * into a `$state([])` slot and every view iterates it, so the wrapper reached
 * `Dashboard.svelte` as `hotspots.slice is not a function` and the other four
 * as `not iterable`. Same defect and same fix as `unwrapPalaceList` in
 * `ui-memory` (#7083): the unwrap belongs here and not in the console bridge,
 * which maps method NAMES and leaves payloads alone.
 *
 * This is not a #6155 regression. The retired axum router wired
 * `GET /indexes/{id}/smells` to this same handler function, so the envelope
 * predates the bridge and these five views have never rendered.
 *
 * What: the list under `key`, or the payload itself when it is already a bare
 * array — so a daemon predating the envelope still renders. Anything else is an
 * empty array rather than a throw: a view rendering "none" beats a view that
 * stalls on an exception.
 *
 * The envelope's other fields are dropped, each for one of two reasons. Echoes
 * of the caller's own argument (`index_id`, `top_n`, `offset`, `limit`,
 * `min_severity`, `k`, `method`) tell a caller what it just passed. Counts the
 * array carries itself (`count`, `returned`) equal `.length`. Three fields are
 * neither — `smells.total`, `smells.truncated` and `clusters.chunk_count` — and
 * no view reads any of them; see the `smells` wrapper for the one whose loss
 * has a consequence.
 * @param {unknown} payload The parsed response body.
 * @param {string} key The envelope field holding the list.
 * @returns {Array<unknown>} The list, flat.
 */
export function unwrapList(payload, key) {
  if (Array.isArray(payload)) return payload;
  const rows = payload?.[key];
  return Array.isArray(rows) ? rows : [];
}

/**
 * Why: `SmellItem.smells` carries `CodeSmell`, a Rust enum serde renders
 * externally tagged — `{"LongFunction": {"lines": 80}}` for a variant with
 * fields and the bare string `"MissingDocstring"` for one without. `Smells.svelte`
 * groups by a category name, and neither shape has one, so every row landed in
 * a single `unknown` bucket.
 * What: the variant name. A string is its own name; an object contributes its
 * one key. `category` and `name` are honoured first so a daemon that ever sends
 * a flat tag still groups correctly.
 * Test: `src/lib/api.test.js`.
 * @param {unknown} smell One entry of a `SmellItem`'s `smells` array.
 * @returns {string} The variant name, or `'unknown'`.
 */
export function smellCategory(smell) {
  if (typeof smell === 'string') return smell;
  if (!smell || typeof smell !== 'object') return 'unknown';
  if (smell.category) return String(smell.category);
  if (smell.name) return String(smell.name);
  const [tag] = Object.keys(smell);
  return tag || 'unknown';
}

export const api = {
  /** Daemon liveness, dependency reachability and version — a flat object. */
  health: () => request('/health'),

  /** The index roster — `analyze.list_indexes` answers a bare array. */
  indexes: () => request('/indexes'),

  /** Ranked hotspot chunks, unwrapped out of `{index_id, top_n, hotspots}`. */
  complexityHotspots: (id, topK = 20) =>
    request(`/indexes/${encodeURIComponent(id)}/complexity_hotspots?top_k=${topK}`).then((r) =>
      unwrapList(r, 'hotspots')
    ),

  /**
   * Smelly chunks, unwrapped out of the pagination envelope.
   *
   * The envelope's `total` and `truncated` are dropped with the rest: the
   * daemon's page size is 500 (`analysis::default_limit`) and this wrapper
   * sends no `limit`, so a corpus with more than 500 smelly chunks is silently
   * shown as its first 500. No view reads either field today, and adding the
   * "showing 500 of N" banner that would close that gap is a change to
   * `Smells.svelte`'s markup, not to this wrapper's shape.
   */
  smells: (id, category) =>
    request(
      `/indexes/${encodeURIComponent(id)}/smells${category ? '?category=' + encodeURIComponent(category) : ''}`
    ).then((r) => unwrapList(r, 'chunks')),

  /** One index's aggregate quality — `QualityReport`, a flat object. */
  quality: (id) => request(`/indexes/${encodeURIComponent(id)}/quality`),

  /** Ranked suggestions, unwrapped out of `{index_id, count, …, suggestions}`. */
  refactorSuggestions: (id, { minSeverity = 'low', topK = 20 } = {}) => {
    const qs = new URLSearchParams({
      min_severity: minSeverity,
      top_k: String(topK)
    });
    return request(`/indexes/${encodeURIComponent(id)}/refactor-suggestions?${qs}`).then((r) =>
      unwrapList(r, 'suggestions')
    );
  },

  /** Concept clusters, unwrapped out of `ClusterResponse`. */
  clusters: (id, { k = 8, method = 'bow' } = {}) =>
    request(`/indexes/${encodeURIComponent(id)}/clusters?k=${k}&method=${method}`).then((r) =>
      unwrapList(r, 'clusters')
    ),

  /** Matching facts, unwrapped out of `{facts, count}`. */
  listFacts: (subject, predicate) => {
    const qs = new URLSearchParams();
    if (subject) qs.set('subject', subject);
    if (predicate) qs.set('predicate', predicate);
    const tail = qs.toString();
    return request(`/facts${tail ? '?' + tail : ''}`).then((r) => unwrapList(r, 'facts'));
  },

  /** Write one fact — answers `{id, upserted}`, which no caller reads. */
  upsertFact: (fact) => request('/facts', { method: 'POST', body: JSON.stringify(fact) }),

  /** Remove one fact — answers `{id, removed}`, which no caller reads. */
  deleteFact: (id) => request(`/facts/${encodeURIComponent(id)}`, { method: 'DELETE' })
};
