/**
 * Which view one hash route resolves to.
 *
 * Why (#6941): the dispatch used to sit inline in `App.svelte`, where its ORDER
 * is the whole contract and nothing could assert it. The roster arm matches on
 * `segments[0] === 'indexes'` alone, so any two-segment `#/indexes/*` route
 * added after it renders the roster instead — silently, with no error anywhere.
 * The stale-registration panel is exactly such a route, so the precedence is
 * pulled out here where a test can hold it.
 *
 * What: a pure function from the router's `segments` array to a view
 * descriptor. No state, no DOM.
 * Test: `routes.test.js`.
 */

/**
 * @param {string[]} segments Path segments, as `router.svelte.js` parses them.
 * @returns {{kind: string, id?: string}} The view to render.
 */
export function resolveView(segments) {
  const segs = Array.isArray(segments) ? segments : [];
  if (segs.length === 0) return { kind: 'dashboard' };
  if (segs[0] === 'search') return { kind: 'search' };
  const isIndexes = segs[0] === 'indexes' || segs[0] === 'index';
  // Drill-down: #/indexes/<id>/config → per-index hygiene settings (#1372).
  if (isIndexes && segs.length >= 3 && segs[2] === 'config') {
    return { kind: 'index-config', id: decodeURIComponent(segs[1]) };
  }
  // #6941: the stale-registration prune / unjudged-deregister panel. Matched
  // BEFORE the roster arm, which would otherwise swallow it. An index literally
  // named `cleanup` keeps its own screen at the three-segment form above.
  if (isIndexes && segs.length === 2 && segs[1] === 'cleanup') {
    return { kind: 'cleanup' };
  }
  if (isIndexes) return { kind: 'indexes' };
  if (segs[0] === 'config') return { kind: 'config' };
  if (segs[0] === 'health') return { kind: 'health' };
  if (segs[0] === 'logs') return { kind: 'logs' };
  return { kind: 'dashboard' };
}
