/*
 * Why: the search box offers suggestions while the operator types. The
 * daemon's typeahead is per index (`GET /indexes/{id}/typeahead`, lexical
 * mode answers in well under 30 ms with no embedding call), and the search box
 * searches every index, so suggestions fan out across the roster.
 * What: `fanOutTypeahead` asks each index with bounded concurrency and merges
 * the hits by score; `createTypeahead` debounces keystrokes, aborts the
 * request a newer keystroke supersedes, and drops any answer that arrives
 * after a newer one was asked for.
 * Test: `typeahead.test.js`, `SearchBox.test.js`.
 */

import { api } from './api.js';

/** Keystroke debounce before a suggestion request is sent. */
export const TYPEAHEAD_DELAY_MS = 150;
/** Suggestions shown in the list. */
export const MAX_SUGGESTIONS = 8;
/** Suggestions asked of each index. */
const PER_INDEX_LIMIT = 4;
/** Indexes asked at once; keeps one keystroke from flooding the daemon's admission limit. */
const FANOUT_CONCURRENCY = 4;

/**
 * Why: a 501 means the server in front of the daemon has no route for
 * typeahead (the console bridge before it maps `search.typeahead`). Asking
 * again on every keystroke cannot succeed, so the caller stops asking.
 * What: true for an error carrying HTTP status 501.
 * @param {unknown} e
 */
export function isUnroutable(e) {
  return /** @type {{status?: number}} */ (e)?.status === 501;
}

/**
 * Why: hits from several indexes need one ranked list with no repeats.
 * What: flattens, drops duplicates (same index, path and line), sorts by
 * score descending, keeps the first `limit`.
 * Test: `mergeSuggestions ranks across indexes and drops duplicates`.
 * @param {Array<Array<object>>} perIndex
 * @param {number} [limit]
 */
export function mergeSuggestions(perIndex, limit = MAX_SUGGESTIONS) {
  const seen = new Set();
  const out = [];
  for (const hit of perIndex.flat().sort((a, b) => (b.score ?? 0) - (a.score ?? 0))) {
    const key = `${hit.index_id}\0${hit.path}\0${hit.start_line}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(hit);
    if (out.length === limit) break;
  }
  return out;
}

/**
 * Why: one keystroke must not open one request per index all at once.
 * What: asks `indexIds` at most `FANOUT_CONCURRENCY` at a time, tags each hit
 * with its `index_id`, and merges. A failing index is skipped; when every
 * index fails, the first error is rethrown so a 501 reaches the caller.
 * Test: `fanOutTypeahead tags hits and rethrows when every index fails`.
 * @param {string[]} indexIds
 * @param {string} q
 * @param {AbortSignal} signal
 */
export async function fanOutTypeahead(indexIds, q, signal) {
  const results = [];
  const errors = [];
  let next = 0;
  async function worker() {
    while (next < indexIds.length && !signal.aborted) {
      const id = indexIds[next++];
      try {
        const body = await api.typeahead(id, q, PER_INDEX_LIMIT, signal);
        results.push((body?.hits ?? []).map((h) => ({ ...h, index_id: id })));
      } catch (e) {
        errors.push(e);
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(FANOUT_CONCURRENCY, indexIds.length) }, worker));
  if (results.length === 0 && errors.length > 0) throw errors[0];
  return mergeSuggestions(results);
}

/**
 * Why: typing fast must cost one request per pause, and a slow answer to an
 * old prefix must never replace the list for the current one.
 * What: `schedule(q)` restarts the debounce, aborts the in-flight request and
 * invalidates its answer; after `delay` ms it calls `load(q, signal)` and
 * hands the hits to `onResults` only if no newer `schedule`/`cancel` came in.
 * A blank `q` clears the list immediately with no request.
 * Test: `SearchBox.test.js` — debounce and stale-response cases.
 * @param {{
 *   load: (q: string, signal: AbortSignal) => Promise<Array<object>>,
 *   onResults: (hits: Array<object>) => void,
 *   onError?: (e: unknown) => void,
 *   delay?: number,
 * }} opts
 */
export function createTypeahead({ load, onResults, onError = () => {}, delay = TYPEAHEAD_DELAY_MS }) {
  let timer = null;
  let controller = null;
  let generation = 0;

  function cancel() {
    generation++;
    clearTimeout(timer);
    timer = null;
    controller?.abort();
    controller = null;
  }

  function schedule(q) {
    cancel();
    if (!q.trim()) {
      onResults([]);
      return;
    }
    const mine = generation;
    timer = setTimeout(async () => {
      const ac = new AbortController();
      controller = ac;
      try {
        const hits = await load(q, ac.signal);
        if (mine === generation) onResults(hits);
      } catch (e) {
        if (mine === generation && !ac.signal.aborted) onError(e);
      }
    }, delay);
  }

  return { schedule, cancel };
}
