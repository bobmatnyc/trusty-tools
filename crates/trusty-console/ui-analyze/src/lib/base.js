// KEEP IN SYNC WITH crates/trusty-console/ui-memory/src/lib/base.js and
// crates/trusty-console/ui-search/src/lib/base.js (#6155 moved all three SPAs'
// source into trusty-console).
/*
 * Why: #6155 — the console serves this SPA at `/tools/analyze/` and bridges its
 * API calls under `/api/analyze/`. Absolute fetch paths like `/health` or
 * `/indexes` would otherwise resolve to the console host root instead of that
 * bridge prefix.
 * This helper derives the correct base URL from the document's actual location
 * so that all API calls work both when served directly by the daemon
 * (base = origin/) and when served under a proxy sub-path
 * (base = origin/proxy/analyze/).
 * What: Returns an absolute base URL string by snapshotting document.baseURI
 * once at module load (before any navigation), then stripping a trailing
 * `index.html` and a trailing `ui/` path segment. Checks
 * `window.__ANALYZE_BASE__` first so the console's injected global wins.
 * Test: In a browser at http://127.0.0.1:7788/tools/analyze/ the return value
 * is the injected "http://127.0.0.1:7788/api/analyze/". Verify by opening
 * /tools/analyze/ and confirming api.health() fetches /api/analyze/health.
 *
 * NOTE on the `ui/` strip (issue #1329): trusty-analyze used to mount this SPA
 * at the daemon's `/ui/` route while the API endpoints were siblings at the
 * parent root, so the strip was load-bearing then. #6287 deleted that listener
 * and the console mounts the SPA at `/tools/analyze/`, where the injected
 * global answers first and the strip is a no-op. It is kept identical across
 * all three files to honour the KEEP IN SYNC contract.
 *
 * NOTE: The base is snapshotted once at module-init time (see API_BASE
 * below). All three SPAs use hash-based routing, so location.pathname never
 * changes after load — but snapshotting makes the helper robust if that
 * ever changes.
 */

/**
 * Compute the base URL once from the current document location.
 * Checks (in order):
 * 1. `window.__ANALYZE_BASE__` (injected by the console at `/tools/analyze/`).
 * 2. The `document.baseURI` PATHNAME (fragment and query excluded — #4980)
 *    with trailing `index.html` and trailing `ui/` stripped, re-joined to the
 *    origin.
 * 3. "/" as a final fallback for non-browser environments.
 * @returns {string}
 */
function computeBase() {
  if (typeof window !== 'undefined' && window.__ANALYZE_BASE__) {
    const b = window.__ANALYZE_BASE__;
    return b.endsWith('/') ? b : b + '/';
  }
  if (typeof document === 'undefined') {
    return '/';
  }
  // #4980: strip against the parsed pathname, not the raw href — baseURI
  // carries the fragment, so at `…/ui/#/` the `$`-anchored strips no-op.
  const u = new URL(document.baseURI);
  // 1. Strip a trailing "index.html" so the base always ends with "/".
  // 2. Strip the trailing "ui/" mount segment (a no-op under the console's
  //    /tools/analyze/ mount; kept for the #1329 contract above).
  const path = u.pathname
    .replace(/index\.html$/, '')
    .replace(/(^|\/)ui\/$/, '$1');
  return u.origin + path;
}

// Snapshot the base once at module load. This runs before any client-side
// navigation, guaranteeing the proxy sub-path is captured correctly even if
// routing ever switches to pathname-based navigation in the future.
const API_BASE = computeBase();

/**
 * Returns the snapshotted base URL for API calls.
 * @returns {string}
 */
export function apiBase() {
  return API_BASE;
}

/**
 * Resolves an API path relative to the derived base URL.
 * Paths starting with "/" are treated as relative to the base, NOT to the
 * origin, so "/health" under base "http://host/proxy/analyze/" becomes
 * "http://host/proxy/analyze/health".
 * @param {string} path  Absolute-looking path, e.g. "/health" or "/indexes"
 * @returns {string}     Fully-qualified URL string
 */
export function apiUrl(path) {
  const rel = path.startsWith('/') ? path.slice(1) : path;
  return new URL(rel, API_BASE).href;
}
