// KEEP IN SYNC WITH crates/trusty-{memory,search}/ui/src/lib/base.js
/*
 * Why: The daemon serves this SPA under the `/ui/` mount (routes `/ui/` and
 * `/ui/{*path}` in src/service/routes.rs) while the JSON API endpoints
 * (`/health`, `/indexes`, the SSE streams, …) live as SIBLINGS at the daemon
 * ROOT — one level ABOVE `ui/`. Earlier code (PR #996) derived the API base
 * straight from `document.baseURI`, which at `…/ui/` is the `/ui/` directory
 * itself; a path like `/health` then resolved to `…/ui/health`, which the
 * daemon answers with the SPA's index.html (text/html) instead of the JSON
 * API. That produced the offline badge of #1329. The fix is to strip the
 * trailing `ui/` segment so the base points at the API root that is the parent
 * of the SPA mount.
 *
 * This also preserves the trusty-console reverse-proxy case (PR #996's
 * original intent). The console mounts `/proxy/{daemon}/{*path}` and forwards
 * `{*path}` verbatim to the daemon root (see trusty-console
 * src/proxy/routes.rs). So when the SPA is opened at
 * `https://console/proxy/analyze/ui/`, stripping the trailing `ui/` yields the
 * API base `https://console/proxy/analyze/`; a `/health` fetch then resolves to
 * `https://console/proxy/analyze/health`, which the proxy rewrites to the
 * daemon's `/health`. Both the direct and proxied cases collapse to the same
 * rule: API base = document.baseURI with its trailing `ui/` segment removed.
 *
 * What: Returns an absolute base URL string by snapshotting document.baseURI
 * once at module load (before any navigation), then stripping a trailing
 * `index.html` and a trailing `ui/` path segment. The legacy
 * `window.__ANALYZER_BASE__` override is still honoured if set, so any existing
 * deployment that injects that global keeps working.
 * Test: Unit-covered in base.test.js. In a browser at
 * http://127.0.0.1:7879/ui/ the API base should be "http://127.0.0.1:7879/"
 * and api.health() should fetch http://127.0.0.1:7879/health (NOT
 * .../ui/health). Behind the console at
 * https://console/proxy/analyze/ui/ the base should be
 * "https://console/proxy/analyze/".
 *
 * NOTE: The base is snapshotted once at module-init time (see API_BASE
 * below). All three SPAs use hash-based routing, so location.pathname never
 * changes after load — but snapshotting makes the helper robust if that
 * ever changes.
 */

/**
 * Compute the base URL once from the current document location.
 * Checks (in order):
 * 1. `window.__ANALYZER_BASE__` (legacy override, kept for backward-compat).
 * 2. `document.baseURI` with trailing `index.html` and trailing `ui/` stripped.
 * 3. "/" as a final fallback for non-browser environments.
 * @returns {string}
 */
function computeBase() {
  if (typeof window !== 'undefined' && window.__ANALYZER_BASE__) {
    const b = window.__ANALYZER_BASE__;
    return b.endsWith('/') ? b : b + '/';
  }
  if (typeof document === 'undefined') {
    return '/';
  }
  // 1. Strip a trailing "index.html" so the base always ends with "/".
  // 2. Strip the trailing "ui/" mount segment: the daemon serves the SPA at
  //    `/ui/` but the API endpoints are siblings at the parent (issue #1329).
  return document.baseURI
    .replace(/index\.html$/, '')
    .replace(/(^|\/)ui\/$/, '$1');
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
